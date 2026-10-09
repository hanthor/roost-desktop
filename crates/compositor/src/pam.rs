//! Lock-screen authentication through PAM (#62, the 004 gate).
//!
//! Under GDM (TunaOS Marlin's login manager) no greetd socket exists,
//! so the greetd unlock path alone would leave a locked session locked
//! for good. This is the other path, the one swaylock uses: a PAM
//! transaction for the session's own user against the `tuna-lock`
//! service (`/etc/pam.d/tuna-lock`, shipped with the package, which
//! includes `login`). Checking one's own password needs no privilege:
//! pam_unix hands it to its setuid helper.
//!
//! libpam is loaded at run time, so builds need no PAM headers and a
//! system without libpam fails closed. [`PamClient`] answers the same
//! [`UnlockClient`](crate::unlock::UnlockClient) seam as greetd, so the
//! prompt flow and its silent-failure rules stay in one place. The
//! transaction runs on a worker thread with a deadline: a PAM stack that
//! never returns cannot hang the compositor past it.

use std::ffi::{c_char, c_int, c_void, CString};
use std::sync::mpsc;
use std::time::Duration;

use greetd_ipc::{codec::Error as CodecError, AuthMessageType, ErrorType, Response};

/// PAM service the lock screen authenticates against.
pub const DEFAULT_SERVICE: &str = "tuna-lock";
/// Environment override for the service name (tests, distributions).
pub const SERVICE_ENV: &str = "TUNA_PAM_SERVICE";
/// Longest the compositor waits for one PAM transaction.
pub const DEADLINE: Duration = Duration::from_secs(10);

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_CONV_ERR: c_int = 19;
const PAM_SILENT: c_int = 0x8000;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(
    c_int,
    *mut *const PamMessage,
    *mut *mut PamResponse,
    *mut c_void,
) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

type PamStart =
    unsafe extern "C" fn(*const c_char, *const c_char, *const PamConv, *mut *mut c_void) -> c_int;
type PamFlagsFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

/// Credentials the conversation hands out, zeroed on drop.
struct Secrets {
    user: CString,
    password: CString,
}

impl Drop for Secrets {
    fn drop(&mut self) {
        let bytes = std::mem::take(&mut self.password).into_bytes_with_nul();
        let mut bytes = bytes;
        for b in bytes.iter_mut() {
            // Volatile so the wipe is not optimized away.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

/// The PAM conversation: the password for hidden prompts, the user name
/// for visible ones, nothing for messages. Anything else fails it.
unsafe extern "C" fn conversation(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if count <= 0 || messages.is_null() || responses.is_null() || appdata.is_null() {
        return PAM_CONV_ERR;
    }
    let secrets = &*(appdata as *const Secrets);
    let count = count as usize;
    let out = libc::calloc(count, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if out.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..count {
        // Linux-PAM passes an array of pointers to messages.
        let message = *messages.add(i);
        if message.is_null() {
            free_responses(out, i);
            return PAM_CONV_ERR;
        }
        let reply = match (*message).msg_style {
            PAM_PROMPT_ECHO_OFF => libc::strdup(secrets.password.as_ptr()),
            PAM_PROMPT_ECHO_ON => libc::strdup(secrets.user.as_ptr()),
            PAM_ERROR_MSG | PAM_TEXT_INFO => std::ptr::null_mut(),
            _ => {
                free_responses(out, i);
                return PAM_CONV_ERR;
            }
        };
        (*out.add(i)).resp = reply;
        (*out.add(i)).resp_retcode = 0;
    }
    *responses = out;
    PAM_SUCCESS
}

/// Free the first `filled` responses (wiping them) and the array.
unsafe fn free_responses(out: *mut PamResponse, filled: usize) {
    for i in 0..filled {
        let resp = (*out.add(i)).resp;
        if !resp.is_null() {
            let len = libc::strlen(resp);
            std::ptr::write_bytes(resp, 0, len);
            libc::free(resp as *mut c_void);
        }
    }
    libc::free(out as *mut c_void);
}

/// Authenticate `user` with `password` against `service`: auth, then
/// account checks (expired or locked accounts stay locked). Any failure,
/// including a missing libpam, is `false`.
pub fn authenticate(service: &str, user: &str, password: &str) -> bool {
    let (Ok(service), Ok(user_c), Ok(password)) = (
        CString::new(service),
        CString::new(user),
        CString::new(password),
    ) else {
        return false;
    };
    let secrets = Box::new(Secrets {
        user: user_c.clone(),
        password,
    });
    unsafe {
        // Load libpam into the global scope and resolve symbols there,
        // as a program linked against it would: preloaded wrappers
        // (pam_wrapper in tests) interpose exactly as they do for it.
        use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
        let Ok(_libpam) = Library::open(Some("libpam.so.0"), RTLD_NOW | RTLD_GLOBAL) else {
            return false;
        };
        let lib = Library::this();
        let (Ok(start), Ok(auth), Ok(acct), Ok(end)) = (
            lib.get::<PamStart>(b"pam_start\0"),
            lib.get::<PamFlagsFn>(b"pam_authenticate\0"),
            lib.get::<PamFlagsFn>(b"pam_acct_mgmt\0"),
            lib.get::<PamFlagsFn>(b"pam_end\0"),
        ) else {
            return false;
        };
        let conv = PamConv {
            conv: conversation,
            appdata_ptr: &*secrets as *const Secrets as *mut c_void,
        };
        let mut handle: *mut c_void = std::ptr::null_mut();
        if start(service.as_ptr(), user_c.as_ptr(), &conv, &mut handle) != PAM_SUCCESS
            || handle.is_null()
        {
            return false;
        }
        let mut status = auth(handle, PAM_SILENT);
        if status == PAM_SUCCESS {
            status = acct(handle, PAM_SILENT);
        }
        end(handle, status);
        status == PAM_SUCCESS
    }
}

/// [`authenticate`] on a worker thread, bounded by [`DEADLINE`].
pub fn authenticate_with_deadline(service: &str, user: &str, password: &str) -> bool {
    let (tx, rx) = mpsc::channel();
    let (service, user, password) = (service.to_owned(), user.to_owned(), password.to_owned());
    let spawned = std::thread::Builder::new()
        .name("tuna-pam".into())
        .spawn(move || {
            let _ = tx.send(authenticate(&service, &user, &password));
        });
    if spawned.is_err() {
        return false;
    }
    rx.recv_timeout(DEADLINE).unwrap_or(false)
}

/// The service to use: [`SERVICE_ENV`] when set, else [`DEFAULT_SERVICE`].
pub fn service() -> String {
    std::env::var(SERVICE_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SERVICE.to_owned())
}

/// [`UnlockClient`](crate::unlock::UnlockClient) over PAM, speaking the
/// greetd shapes: one hidden prompt, then success or a generic failure.
pub struct PamClient {
    service: String,
    user: Option<String>,
}

impl PamClient {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            user: None,
        }
    }
}

impl crate::unlock::UnlockClient for PamClient {
    fn create_session(&mut self, user: &str) -> Result<Response, CodecError> {
        self.user = Some(user.to_owned());
        Ok(Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password:".to_owned(),
        })
    }

    fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
        let (Some(user), Some(password)) = (self.user.as_deref(), text) else {
            return Ok(Response::Error {
                error_type: ErrorType::AuthError,
                description: String::new(),
            });
        };
        if authenticate_with_deadline(&self.service, user, &password) {
            Ok(Response::Success)
        } else {
            Ok(Response::Error {
                error_type: ErrorType::AuthError,
                description: String::new(),
            })
        }
    }

    fn cancel(&mut self) -> Result<(), CodecError> {
        self.user = None;
        Ok(())
    }
}
