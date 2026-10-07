//! Native output ownership resolved from an actual opened KMS char-device.
//! Connector names alone never establish authority across GPU cards.
use roost_shell_control::NativeOutputInfo;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

fn text(path: &Path) -> Result<String, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("nonregular connector attribute".into());
    }
    let before = file.metadata().map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    file.by_ref()
        .take(65)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let after = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !after.is_file() || (before.dev(), before.ino()) != (after.dev(), after.ino()) {
        return Err("connector attribute identity changed".into());
    }
    if raw.len() > 64 {
        return Err("connector attribute exceeds bound".into());
    }
    String::from_utf8(raw)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}

pub fn device(fd: BorrowedFd<'_>) -> Result<u64, String> {
    device_fd(fd.as_raw_fd())
}
fn device_fd(fd: std::os::fd::RawFd) -> Result<u64, String> {
    let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: borrowed FD remains owned by backend; fstat initializes value on success.
    if unsafe { libc::fstat(fd, value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let value = unsafe { value.assume_init() };
    if value.st_mode & libc::S_IFMT != libc::S_IFCHR {
        return Err("KMS FD is not char device".into());
    }
    Ok(value.st_rdev)
}

/// Resolve the exact connector object under the selected char device's sysfs
/// node. No card enumeration or name-based fallback across devices is allowed.
pub fn resolve(
    sys: &Path,
    drm_device: u64,
    name: &str,
    connector_id: u32,
) -> Result<NativeOutputInfo, String> {
    if connector_id == 0 || name.is_empty() || name.contains('/') {
        return Err("invalid native connector identity".into());
    }
    let major = libc::major(drm_device);
    let minor = libc::minor(drm_device);
    let node = std::fs::canonicalize(sys.join("dev/char").join(format!("{major}:{minor}")))
        .map_err(|e| e.to_string())?;
    // Device authority must also be reflected by the kernel node itself.
    if text(&node.join("dev"))? != format!("{major}:{minor}") {
        return Err("native device sysfs mismatch".into());
    }
    let mut found = None;
    for (index, entry) in std::fs::read_dir(&node)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if index >= 128 {
            return Err("owned device inventory exceeds bound".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let candidate = entry.path();
        // Nonconnector GPU attributes are expected, and cannot confer authority.
        if !candidate.join("connector_id").exists() {
            continue;
        }
        let actual_id = text(&candidate.join("connector_id"))?
            .parse::<u32>()
            .map_err(|e| e.to_string())?;
        if actual_id != connector_id {
            continue;
        }
        let base = node
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or("invalid DRM sysfs node")?;
        if entry.file_name().to_str() != Some(format!("{base}-{name}").as_str()) {
            return Err("connector ID/name mismatch".into());
        }
        let canonical = std::fs::canonicalize(&candidate).map_err(|e| e.to_string())?;
        if canonical.parent() != Some(node.as_path()) {
            return Err("connector escaped owned GPU".into());
        }
        if text(&canonical.join("status"))? != "connected" {
            return Err("owned connector is inactive".into());
        }
        if found.is_some() {
            return Err("duplicate connector ID".into());
        }
        let metadata = std::fs::metadata(&canonical).map_err(|e| e.to_string())?;
        found = Some(NativeOutputInfo {
            name: name.into(),
            drm_device,
            connector_id,
            connector_device: metadata.dev(),
            connector_inode: metadata.ino(),
            connector_sysfs: canonical.to_str().ok_or("nonUTF8 connector path")?.into(),
        });
    }
    found.ok_or_else(|| "owned connector missing".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;
    fn connector(root: &Path, card: &str, id: u32) -> std::path::PathBuf {
        let node = root.join("devices/drm").join(card);
        std::fs::create_dir_all(&node).unwrap();
        std::fs::write(node.join("dev"), "226:0").unwrap();
        let path = node.join(format!("{card}-eDP-1"));
        std::fs::create_dir(&path).unwrap();
        for (name, value) in [
            ("connector_id", id.to_string()),
            ("status", "connected".into()),
            ("enabled", "enabled".into()),
        ] {
            std::fs::write(path.join(name), value).unwrap();
        }
        node
    }
    #[test]
    fn fd_authority_requires_actual_opened_character_device() {
        assert!(
            device_fd(-1).is_err(),
            "kernel EBADF must withhold authority"
        );
        let file = tempfile::tempfile().unwrap();
        assert!(device(file.as_fd()).is_err());
        let actual = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(device(actual.as_fd()).unwrap(), libc::makedev(1, 3));
    }
    #[test]
    fn selected_kernel_device_does_not_search_other_gpu_for_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let first = connector(tmp.path(), "card0", 39);
        let _other = connector(tmp.path(), "card1", 51);
        let devices = tmp.path().join("dev/char");
        std::fs::create_dir_all(&devices).unwrap();
        symlink(&first, devices.join("226:0")).unwrap();
        let rdev = libc::makedev(226, 0);
        assert_eq!(
            resolve(tmp.path(), rdev, "eDP-1", 39)
                .unwrap()
                .connector_sysfs,
            first.join("card0-eDP-1").to_str().unwrap()
        );
        assert!(resolve(tmp.path(), rdev, "eDP-1", 51).is_err());
        std::fs::write(first.join("card0-eDP-1/status"), "disconnected").unwrap();
        assert!(resolve(tmp.path(), rdev, "eDP-1", 39).is_err());
        std::fs::remove_file(devices.join("226:0")).unwrap();
        assert!(resolve(tmp.path(), rdev, "eDP-1", 39).is_err());
    }
}
