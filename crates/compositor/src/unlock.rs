//! Greetd unlock path (session-lock, unlock task).
//!
//! Credential entry on the lock surface is verified through the greeter
//! crate's login machinery — [`GreeterClient`] for the greetd
//! conversation and [`GreeterModel`] for the prompt state machine —
//! never through a parallel password check. There is no live daemon in
//! CI, so [`UnlockClient`] is the seam tests stub at; production passes
//! the real [`GreeterClient`], connected to `$GREETD_SOCK` by the
//! runtime. When no daemon is reachable the attempt fails closed: the
//! session stays locked and silent.
//!
//! Unlock authenticates only: on success the pending greetd
//! conversation is cancelled (never started — this session is already
//! running) and the compositor-owned flag clears, restoring the intact
//! session on the next hub poll. Failures — bad credentials, daemon
//! errors, transport drops, endless prompts — all collapse to
//! [`UnlockOutcome::Denied`], which carries no detail by construction.
//! Secrets are never logged and daemon text never surfaces.
//!
//! [`GreeterClient`]: tuna_greeter_control::GreeterClient
//! [`GreeterModel`]: tuna_greeter_control::GreeterModel

use greetd_ipc::{codec::Error as CodecError, Response};
use tuna_greeter_control::{GreeterClient, GreeterModel, ModelEvent};

use crate::control::ControlHub;
use crate::lock::SessionLock;
use crate::overlay::Overlay;

/// Environment variable holding the live greetd socket path.
pub const GREETD_SOCKET_ENV: &str = "GREETD_SOCK";

/// Upper bound on daemon prompt rounds per unlock attempt. A daemon
/// that keeps prompting (misconfiguration or abuse) denies instead of
/// looping forever.
pub const MAX_AUTH_ROUNDS: usize = 8;

/// Outcome of one unlock attempt. `Denied` covers every failure mode
/// — wrong credentials, daemon errors, transport drops — and carries
/// no detail, so failures stay silent and leak nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOutcome {
    /// The daemon authenticated the credentials.
    Unlocked,
    /// Anything else: stay locked, say nothing.
    Denied,
}

/// The client seam [`attempt_unlock`] drives. [`GreeterClient`] is the
/// production implementation; tests stub here (no live greetd in CI).
pub trait UnlockClient {
    /// Open a login attempt for `user`; the daemon's first response.
    fn create_session(&mut self, user: &str) -> Result<Response, CodecError>;
    /// Answer the last auth message (`None` acknowledges info/error
    /// messages that carry no question).
    fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError>;
    /// Release a successful attempt without starting a session.
    fn cancel(&mut self) -> Result<(), CodecError>;
}

impl UnlockClient for GreeterClient {
    fn create_session(&mut self, user: &str) -> Result<Response, CodecError> {
        GreeterClient::create_session(self, user)
    }

    fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
        GreeterClient::answer(self, text)
    }

    fn cancel(&mut self) -> Result<(), CodecError> {
        GreeterClient::cancel(self)
    }
}

/// Live greetd socket path from the environment, if configured.
pub fn greetd_socket_path() -> Option<String> {
    std::env::var(GREETD_SOCKET_ENV)
        .ok()
        .filter(|path| !path.is_empty())
}

/// Username to authenticate as when unlocking: the session owner's
/// login name from the environment.
pub fn unlock_user() -> Option<String> {
    std::env::var("LOGNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|user| !user.is_empty())
}

/// Verify one entered password through the greeter login path: open a
/// greetd attempt for `user`, answer each prompt with the password
/// (info/error notices auto-acknowledge exactly like login), and
/// report the outcome. Success cancels the pending conversation —
/// this session is already running, so nothing starts.
///
/// Every failure collapses to [`UnlockOutcome::Denied`] with no
/// detail: wrong credentials, daemon errors, transport failures, and
/// prompt loops beyond [`MAX_AUTH_ROUNDS`].
pub fn attempt_unlock(client: &mut impl UnlockClient, user: &str, password: &str) -> UnlockOutcome {
    let mut model = GreeterModel::new();
    model.begin_user(user);
    let mut response = match client.create_session(user) {
        Ok(response) => response,
        Err(_) => return UnlockOutcome::Denied,
    };
    for _ in 0..MAX_AUTH_ROUNDS {
        match model.apply_response(&response) {
            ModelEvent::NeedAnswer { .. } => {
                response = match client.answer(Some(password.to_string())) {
                    Ok(response) => response,
                    Err(_) => return UnlockOutcome::Denied,
                };
            }
            ModelEvent::AutoAck => {
                response = match client.answer(None) {
                    Ok(response) => response,
                    Err(_) => return UnlockOutcome::Denied,
                };
            }
            ModelEvent::Authenticated => {
                // Authenticated only: release the pending attempt
                // without starting anything; best-effort, the daemon
                // drops it on disconnect either way.
                let _ = client.cancel();
                return UnlockOutcome::Unlocked;
            }
            ModelEvent::Failed => return UnlockOutcome::Denied,
        }
    }
    UnlockOutcome::Denied
}

/// Verify `password` for `user` the way [`unlock_session`] does, without
/// touching any lock state: greetd's daemon when one runs, else PAM's
/// `tuna-lock` service (GDM sessions). Blocking (PAM may delay a
/// failure for seconds), so the runtime calls it on a worker thread.
pub fn verify(user: &str, password: &str) -> bool {
    if let Some(path) = greetd_socket_path() {
        let Ok(mut client) = GreeterClient::connect(&path) else {
            return false;
        };
        return matches!(
            attempt_unlock(&mut client, user, password),
            UnlockOutcome::Unlocked
        );
    }
    let mut client = crate::pam::PamClient::new(crate::pam::service());
    matches!(
        attempt_unlock(&mut client, user, password),
        UnlockOutcome::Unlocked
    )
}

/// The session's user name, for unlocking.
pub fn session_user() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage, read at once.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

/// Apply one unlock attempt to the compositor-owned lock state: verify
/// `password` for `user` through `client`, and on success clear the
/// idle flag plus the hub mirror (the next poll restores the intact
/// session) and drop the lock surface. Returns whether the session is
/// unlocked afterwards.
///
/// An already-unlocked session returns true without contacting the
/// daemon — nothing is protected, so no credential is needed.
/// Failures touch nothing: the flag, the hub mirror, and the surface
/// stay exactly as they were, silently.
pub fn unlock_session(
    lock: &mut SessionLock,
    hub: &ControlHub,
    overlay: &mut Overlay,
    now_ms: u64,
    user: &str,
    password: &str,
    client: &mut impl UnlockClient,
) -> bool {
    if !lock.is_locked() && !hub.is_locked() {
        return true;
    }
    if matches!(
        attempt_unlock(client, user, password),
        UnlockOutcome::Unlocked
    ) {
        // Restart the idle accumulator at now so the session does not
        // relock on the next tick; the hub poll carries the restore.
        lock.unlock(now_ms);
        hub.set_locked(false);
        overlay.hide();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greetd_ipc::{AuthMessageType, ErrorType};
    use std::collections::VecDeque;

    fn secret_prompt(text: &str) -> Response {
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: text.to_string(),
        }
    }

    fn info_notice(text: &str) -> Response {
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Info,
            auth_message: text.to_string(),
        }
    }

    fn auth_error() -> Response {
        Response::Error {
            error_type: ErrorType::AuthError,
            description: "PAM authentication failed".to_string(),
        }
    }

    /// Scripted seam stub: canned replies for `create_session` then
    /// each `answer`, recording every answer verbatim.
    struct ScriptStub {
        replies: VecDeque<Result<Response, CodecError>>,
        answers: Vec<Option<String>>,
        cancelled: bool,
    }

    impl ScriptStub {
        fn new(replies: Vec<Result<Response, CodecError>>) -> Self {
            Self {
                replies: replies.into(),
                answers: Vec::new(),
                cancelled: false,
            }
        }

        fn next_reply(&mut self) -> Result<Response, CodecError> {
            self.replies.pop_front().unwrap_or(Err(CodecError::Eof))
        }
    }

    impl UnlockClient for ScriptStub {
        fn create_session(&mut self, _user: &str) -> Result<Response, CodecError> {
            self.next_reply()
        }

        fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
            self.answers.push(text);
            self.next_reply()
        }

        fn cancel(&mut self) -> Result<(), CodecError> {
            self.cancelled = true;
            Ok(())
        }
    }

    #[test]
    fn correct_password_unlocks_and_releases_the_attempt() {
        let mut stub = ScriptStub::new(vec![Ok(secret_prompt("Password:")), Ok(Response::Success)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Unlocked
        );
        // The entered credential is what the conversation carried.
        assert_eq!(stub.answers, vec![Some("s3cret".to_string())]);
        // Success never starts a session: the pending attempt is out.
        assert!(stub.cancelled);
    }

    #[test]
    fn wrong_password_denies_without_release() {
        let mut stub = ScriptStub::new(vec![Ok(secret_prompt("Password:")), Ok(auth_error())]);
        assert_eq!(
            attempt_unlock(&mut stub, "mallory", "wrong"),
            UnlockOutcome::Denied
        );
        assert!(!stub.cancelled);
    }

    #[test]
    fn daemon_unreachable_denies_silently() {
        let mut stub = ScriptStub::new(vec![Err(CodecError::Eof)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
        assert!(stub.answers.is_empty());
    }

    #[test]
    fn mid_conversation_drop_denies() {
        let mut stub = ScriptStub::new(vec![Ok(secret_prompt("Password:")), Err(CodecError::Eof)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
    }

    #[test]
    fn endless_prompts_deny_bounded() {
        let mut stub = ScriptStub::new(
            (0..MAX_AUTH_ROUNDS + 4)
                .map(|_| Ok(secret_prompt("Password:")))
                .collect(),
        );
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Denied
        );
        assert_eq!(stub.answers.len(), MAX_AUTH_ROUNDS);
    }

    #[test]
    fn info_notice_auto_acknowledges_like_login() {
        let mut stub = ScriptStub::new(vec![Ok(info_notice("Hello.")), Ok(Response::Success)]);
        assert_eq!(
            attempt_unlock(&mut stub, "ada", "s3cret"),
            UnlockOutcome::Unlocked
        );
        // Info carries no question: acknowledged empty, unprompted.
        assert_eq!(stub.answers, vec![None]);
    }

    #[test]
    fn unlock_user_reads_session_owner() {
        let outer_logname = std::env::var_os("LOGNAME");
        let outer_user = std::env::var_os("USER");
        unsafe {
            std::env::set_var("LOGNAME", "ada");
            std::env::remove_var("USER");
        }
        assert_eq!(unlock_user().as_deref(), Some("ada"));
        unsafe {
            std::env::remove_var("LOGNAME");
            std::env::set_var("USER", "grace");
        }
        assert_eq!(unlock_user().as_deref(), Some("grace"));
        unsafe {
            std::env::remove_var("LOGNAME");
            std::env::remove_var("USER");
        }
        assert_eq!(unlock_user(), None);
        unsafe {
            match outer_logname {
                Some(value) => std::env::set_var("LOGNAME", value),
                None => std::env::remove_var("LOGNAME"),
            }
            match outer_user {
                Some(value) => std::env::set_var("USER", value),
                None => std::env::remove_var("USER"),
            }
        }
    }
}
