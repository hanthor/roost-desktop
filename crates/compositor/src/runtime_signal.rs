//! Small regular-file signals consumed by the compositor loop.
//!
//! Reject special files before reading, without waiting for a FIFO peer. This
//! bounds signal size and does not claim regular-file storage I/O is async.
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const MAX_BYTES: u64 = 4096;

pub(crate) fn read(path: &Path) -> io::Result<String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime signal must be a small regular file",
        ));
    }
    // The file can grow after metadata inspection. Limit the descriptor read
    // too, with one extra byte to detect overflow rather than accept a prefix.
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime signal exceeds its size limit",
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_runtime_signals_follow_atomic_policy_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tuna-idle-blank");
        std::fs::write(&path, "300000\n10000\n").unwrap();
        let policy = crate::lock::IdleBlank::parse(&read(&path).unwrap());
        assert_eq!((policy.idle_ms, policy.fade_ms), (300000, 10000));
        let next = dir.path().join(".tuna-idle-blank.tmp");
        std::fs::write(&next, "120000\n0\n").unwrap();
        std::fs::rename(next, &path).unwrap();
        let policy = crate::lock::IdleBlank::parse(&read(&path).unwrap());
        assert_eq!((policy.idle_ms, policy.fade_ms), (120000, 0));
        std::fs::write(&path, r#"{"phase":"begin","x":0,"y":0}"#).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&read(&path).unwrap()).unwrap()["phase"],
            "begin"
        );
    }

    #[test]
    fn runtime_signal_rejects_a_fifo_without_a_writer_and_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("tuna-idle-blank");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert_eq!(read(&fifo).unwrap_err().kind(), io::ErrorKind::InvalidData);
        // Rejection keeps the existing missing/malformed policy fallback.
        let fallback = crate::lock::IdleBlank::parse(&read(&fifo).unwrap_or_default());
        assert_eq!((fallback.idle_ms, fallback.fade_ms), (0, 0));
        let target = dir.path().join("regular");
        std::fs::write(&target, "300000\n10000\n").unwrap();
        let link = dir.path().join("tuna-swipe-input");
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert!(read(&link).is_err(), "signal must not follow a symlink");
    }

    #[test]
    fn runtime_signal_bounds_bytes_and_rejects_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tuna-swipe-input");
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize]).unwrap();
        assert_eq!(read(&path).unwrap().len(), MAX_BYTES as usize);
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        std::fs::write(&path, [0xff]).unwrap();
        assert_eq!(read(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
