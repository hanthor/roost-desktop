//! GNOME sandbox metadata from an original, live process pin (#410).
//! This is descriptive metadata, never executable/provider/capture authority.
use std::ffi::{c_char, c_void, CStr};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex, OnceLock};

const MAX_INFO: usize = 1024 * 1024;

fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    // A peer namespace must not turn discovery into a blocking FIFO read.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

fn flatpak_id(data: &[u8]) -> Option<String> {
    if data.len() > MAX_INFO {
        return None;
    }
    // Use GNOME's actual GKeyFile grammar (escaping, groups, duplicate keys)
    // without making the standalone compositor link GTK or GLib. Systems
    // without GLib leave optional Flatpak metadata unknown.
    unsafe {
        let library = libloading::Library::new("libglib-2.0.so.0").ok()?;
        let new = library
            .get::<unsafe extern "C" fn() -> *mut c_void>(b"g_key_file_new\0")
            .ok()?;
        let load =
            library
                .get::<unsafe extern "C" fn(
                    *mut c_void,
                    *const c_char,
                    usize,
                    u32,
                    *mut *mut c_void,
                ) -> i32>(b"g_key_file_load_from_data\0")
                .ok()?;
        let get = library
            .get::<unsafe extern "C" fn(
                *mut c_void,
                *const c_char,
                *const c_char,
                *mut *mut c_void,
            ) -> *mut c_char>(b"g_key_file_get_string\0")
            .ok()?;
        let unref = library
            .get::<unsafe extern "C" fn(*mut c_void)>(b"g_key_file_unref\0")
            .ok()?;
        let free = library
            .get::<unsafe extern "C" fn(*mut c_void)>(b"g_free\0")
            .ok()?;
        let key = new();
        if key.is_null() {
            return None;
        }
        let value = if load(
            key,
            data.as_ptr().cast(),
            data.len(),
            0,
            std::ptr::null_mut(),
        ) != 0
        {
            get(
                key,
                c"Application".as_ptr(),
                c"name".as_ptr(),
                std::ptr::null_mut(),
            )
        } else {
            std::ptr::null_mut()
        };
        let result = if value.is_null() {
            None
        } else {
            CStr::from_ptr(value)
                .to_str()
                .ok()
                .filter(|id| !id.is_empty() && id.len() <= 512)
                .map(str::to_owned)
        };
        free(value.cast());
        unref(key);
        result
    }
}

fn snap_id(data: &[u8]) -> Option<String> {
    let profile = std::str::from_utf8(data).ok()?.split_whitespace().next()?;
    let id = profile.strip_prefix("snap.")?;
    (!id.is_empty() && id.len() <= 512).then(|| id.replace('.', "_"))
}

fn discover(credentials: &zbus::fdo::ConnectionCredentials) -> Option<String> {
    let pid = crate::capture_security::pinned_process_id(credentials)?;
    let process = std::path::PathBuf::from(format!("/proc/{pid}"));
    let id = read_bounded(&process.join("root/.flatpak-info"), MAX_INFO)
        .and_then(|data| flatpak_id(&data))
        .or_else(|| {
            read_bounded(&process.join("attr/current"), 4096).and_then(|data| snap_id(&data))
        });
    // If the original process died during filesystem inspection, discard the
    // result. Never open a replacement pidfd from a potentially recycled PID.
    (crate::capture_security::pinned_process_id(credentials) == Some(pid))
        .then_some(id)
        .flatten()
}

type ResultCell = Arc<OnceLock<Option<String>>>;
type Job = (Arc<zbus::fdo::ConnectionCredentials>, ResultCell);

pub(crate) fn discover_async(credentials: Arc<zbus::fdo::ConnectionCredentials>) -> ResultCell {
    // A peer's root can contain a slow FUSE/network filesystem. Never read
    // it in client acceptance, D-Bus admission or the compositor event loop.
    // Bound both blocked workers and queued process descriptors.
    static QUEUE: OnceLock<Option<mpsc::SyncSender<Job>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Job>(32);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = 0;
        for index in 0..4 {
            let receiver = receiver.clone();
            if std::thread::Builder::new()
                .name(format!("sandbox-metadata-{index}"))
                .spawn(move || loop {
                    let job = receiver
                        .lock()
                        .ok()
                        .and_then(|receiver| receiver.recv().ok());
                    let Some((credentials, result)) = job else {
                        break;
                    };
                    let _ = result.set(discover(&credentials));
                })
                .is_ok()
            {
                workers += 1;
            }
        }
        (workers > 0).then_some(sender)
    });
    let result = ResultCell::default();
    if queue
        .as_ref()
        .is_none_or(|queue| queue.try_send((credentials, result.clone())).is_err())
    {
        let _ = result.set(None);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flatpak_uses_actual_keyfile_groups_escaping_and_last_key() {
        assert_eq!(flatpak_id(b"# generated metadata\n[Application]\nname=old\nname=org.gnome.TextEditor\n[Instance]\nname=wrong\n"), Some("org.gnome.TextEditor".into()));
        assert_eq!(
            flatpak_id(b"[Application]\nname=org.example.App\\sinstance\n"),
            Some("org.example.App instance".into())
        );
        for invalid in [
            b"[Runtime]\nname=org.example.Forged\n".as_slice(),
            b"broken file",
            b"[Application]\nname=\n",
            b"[Application]\nname=bad\\q\n",
        ] {
            assert_eq!(flatpak_id(invalid), None);
        }
    }
    #[test]
    fn snap_uses_kernel_profile_not_desktop_app_id_text() {
        assert_eq!(
            snap_id(b"snap.firefox.firefox (enforce)\n"),
            Some("firefox_firefox".into())
        );
        assert_eq!(
            snap_id(b"snap.editor_instance.editor (complain)\n"),
            Some("editor_instance_editor".into())
        );
        for invalid in [
            b"unconfined\n".as_slice(),
            b"org.example.App",
            b"snap. (enforce)",
        ] {
            assert_eq!(snap_id(invalid), None);
        }
    }
    #[test]
    fn bounded_metadata_reader_rejects_oversized_files_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("info");
        std::fs::write(&file, b"original").unwrap();
        assert_eq!(read_bounded(&file, 8), Some(b"original".to_vec()));
        assert_eq!(read_bounded(&file, 7), None);
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read_bounded(&link, 8), None);
    }
    #[test]
    fn namespace_fifo_cannot_block_the_metadata_reader() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(read_bounded(&path, 4096), None);
    }
    #[test]
    fn numeric_process_id_without_original_pin_cannot_discover_identity() {
        let credentials =
            zbus::fdo::ConnectionCredentials::default().set_process_id(std::process::id());
        assert_eq!(discover(&credentials), None);
    }
}
