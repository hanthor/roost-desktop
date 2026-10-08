//! Bounded identity of actual opened native files and FontConfig's declared
//! configuration directories. Directory membership is observed without recursion.
use sha2::{Digest, Sha256};
use std::{
    fs::{Metadata, OpenOptions},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt, fs::OpenOptionsExt},
    path::Path,
};

#[derive(Clone, Copy)]
pub enum Kind {
    File,
    FontConfigDirectory,
}

#[derive(Clone, Copy)]
struct Limits {
    bytes: u64,
    members: usize,
}

pub fn observe(path: &Path, kind: Kind) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    observe_with(
        path,
        kind,
        Limits {
            bytes: super::MAX_INPUT,
            members: 16384,
        },
        || {},
    )
}

type Identity = (u64, u64, u64, u32, u32, i64, i64, i64, i64);
fn identity(m: &Metadata) -> Identity {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.uid(),
        m.mode(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

fn observe_with(
    path: &Path,
    kind: Kind,
    limits: Limits,
    after_read: impl FnOnce(),
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    // Actual FontConfig config/font paths can be symlinks; observe their opened
    // target like the native loader. O_NONBLOCK avoids blocking on special files.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    let mut hash = Sha256::new();
    let mut members = Vec::new();
    let (kind_name, bytes) = match kind {
        Kind::File => {
            if !before.is_file() || before.len() > limits.bytes {
                return Err(format!(
                    "native file resource bound: {} ({} bytes)",
                    path.display(),
                    before.len()
                )
                .into());
            }
            let mut reader = (&file).take(limits.bytes + 1);
            let mut buffer = [0; 16384];
            let mut length = 0u64;
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                length += count as u64;
                if length > limits.bytes {
                    return Err("native file grew beyond bound".into());
                }
                hash.update(&buffer[..count]);
            }
            if length != before.len() {
                return Err("native file length changed".into());
            }
            ("file", length)
        }
        Kind::FontConfigDirectory => {
            if !before.is_dir() {
                return Err("declared FontConfig directory changed type".into());
            }
            // The /proc fd binds enumeration to the opened directory, even if
            // its configured pathname is concurrently replaced. No recursive
            // walk and no following member symlinks is needed for membership.
            use std::os::fd::AsRawFd;
            for entry in std::fs::read_dir(format!("/proc/self/fd/{}", file.as_raw_fd()))? {
                let name = entry?.file_name();
                members.push(name.as_bytes().to_vec());
                if members.len() > limits.members {
                    return Err("FontConfig directory member bound".into());
                }
            }
            members.sort();
            hash.update(b"fontconfig-directory-v1\0");
            hash.update((members.len() as u64).to_le_bytes());
            for name in &members {
                hash.update((name.len() as u64).to_le_bytes());
                hash.update(name);
            }
            ("fontconfig-directory", before.len())
        }
    };
    after_read();
    if identity(&before) != identity(&file.metadata()?)
        || identity(&before) != identity(&path.metadata()?)
    {
        return Err("native resource changed during observation".into());
    }
    Ok(serde_json::json!({
        "path":path.to_string_lossy(), "kind":kind_name, "bytes":bytes,
        "uid":before.uid(), "mode":before.mode(), "dev":before.dev(), "ino":before.ino(),
        "mtime":[before.mtime(),before.mtime_nsec()], "ctime":[before.ctime(),before.ctime_nsec()],
        "sha256":format!("{:x}",hash.finalize()),
        "member_names_hex":members.iter().map(|name| name.iter().map(|b| format!("{b:02x}")).collect::<String>()).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> Limits {
        Limits {
            bytes: 16,
            members: 2,
        }
    }

    #[test]
    fn typed_directory_membership_changes_and_loaded_file_content_remain_visible() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("fonts.conf");
        std::fs::write(&file, b"first").unwrap();
        let before = observe_with(temp.path(), Kind::FontConfigDirectory, limits(), || {}).unwrap();
        let original = observe_with(&file, Kind::File, limits(), || {}).unwrap();
        std::fs::write(&file, b"other").unwrap();
        let changed = observe_with(&file, Kind::File, limits(), || {}).unwrap();
        assert_ne!(original["sha256"], changed["sha256"]);
        std::fs::rename(&file, temp.path().join("renamed.conf")).unwrap();
        let after = observe_with(temp.path(), Kind::FontConfigDirectory, limits(), || {}).unwrap();
        assert_ne!(before["sha256"], after["sha256"]);
        assert_eq!(before["kind"], "fontconfig-directory");
        assert_eq!(before["member_names_hex"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn missing_oversized_and_wrong_type_resources_fail_visibly() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.conf");
        assert!(observe_with(&missing, Kind::File, limits(), || {}).is_err());
        assert!(observe_with(temp.path(), Kind::File, limits(), || {}).is_err());
        let file = temp.path().join("large.conf");
        std::fs::write(&file, b"01234567890123456").unwrap();
        assert!(observe_with(&file, Kind::File, limits(), || {}).is_err());
        assert!(observe_with(&file, Kind::FontConfigDirectory, limits(), || {}).is_err());
        for name in ["two", "three"] {
            std::fs::write(temp.path().join(name), b"").unwrap();
        }
        assert!(observe_with(temp.path(), Kind::FontConfigDirectory, limits(), || {}).is_err());
    }

    #[test]
    fn concurrent_directory_member_change_and_path_replacement_reject_stale_observation() {
        let temp = tempfile::tempdir().unwrap();
        assert!(
            observe_with(temp.path(), Kind::FontConfigDirectory, limits(), || {
                std::fs::write(temp.path().join("new.conf"), b"new").unwrap();
            })
            .is_err()
        );
        let file = temp.path().join("current.conf");
        std::fs::write(&file, b"old").unwrap();
        assert!(observe_with(&file, Kind::File, limits(), || {
            let replacement = temp.path().join("next.conf");
            std::fs::write(&replacement, b"new").unwrap();
            std::fs::rename(replacement, &file).unwrap();
        })
        .is_err());
    }
    #[test]
    fn directory_retains_raw_member_names_and_native_file_symlinks() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let temp = tempfile::tempdir().unwrap();
        let name = std::ffi::OsString::from_vec(b"raw\xff.conf".to_vec());
        let target = temp.path().join(name);
        std::fs::write(&target, b"native").unwrap();
        let receipt =
            observe_with(temp.path(), Kind::FontConfigDirectory, limits(), || {}).unwrap();
        assert_eq!(receipt["member_names_hex"][0], "726177ff2e636f6e66");
        let link = temp.path().join("link.conf");
        symlink(&target, &link).unwrap();
        let original = observe_with(&target, Kind::File, limits(), || {}).unwrap();
        let linked = observe_with(&link, Kind::File, limits(), || {}).unwrap();
        assert_eq!(original["sha256"], linked["sha256"]);
        assert_eq!(original["ino"], linked["ino"]);
        let socket = temp.path().join("special");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(observe_with(&socket, Kind::File, limits(), || {}).is_err());
    }
}
