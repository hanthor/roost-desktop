//! Original bounded raw-backlight/connector observations shared by shell and compositor.
//! These reads confer no backend generation, provider policy or restoration authority.
use roost_shell_control::NativeOutputInfo;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_DEVICES: usize = 64;
const MAX_SCALAR_BYTES: u64 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub output: String,
    pub owner: Option<NativeOutputInfo>,
    pub kind: BacklightKind,
    pub path: PathBuf,
    pub max: u32,
    pub current: u32,
    pub dev: u64,
    pub inode: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacklightKind {
    Raw,
    Firmware,
    Platform,
}
impl BacklightKind {
    pub fn read(path: &Path) -> Result<Self, String> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| e.to_string())?;
        let before = file.metadata().map_err(|e| e.to_string())?;
        if !before.is_file() {
            return Err("backlight type is nonregular".into());
        }
        let mut raw = Vec::new();
        file.by_ref()
            .take(17)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let after = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino())
            || !after.is_file()
            || raw.len() > 16
        {
            return Err("invalid bounded backlight type identity".into());
        }
        match std::str::from_utf8(&raw).map_err(|e| e.to_string())?.trim() {
            "raw" => Ok(Self::Raw),
            "firmware" => Ok(Self::Firmware),
            "platform" => Ok(Self::Platform),
            _ => Err("unknown backlight type".into()),
        }
    }
}

pub fn owner_current(owner: &NativeOutputInfo) -> Result<(), String> {
    let path = Path::new(&owner.connector_sysfs);
    let actual = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
    let meta = std::fs::metadata(&actual).map_err(|e| e.to_string())?;
    if actual != path
        || (meta.dev(), meta.ino()) != (owner.connector_device, owner.connector_inode)
        || scalar(&actual.join("connector_id"))? != owner.connector_id
    {
        return Err("actual owned connector identity changed".into());
    }
    // sysfs text attributes are short regular files; unlike scalar, enabled and
    // status use fixed string domains. No node/card-name-only matching.
    for (name, expected) in [("status", "connected"), ("enabled", "enabled")] {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(actual.join(name))
            .map_err(|e| e.to_string())?;
        let before = file.metadata().map_err(|e| e.to_string())?;
        if !before.is_file() {
            return Err("nonregular connector status".into());
        }
        let mut raw = Vec::new();
        file.by_ref()
            .take(17)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        let after = std::fs::symlink_metadata(actual.join(name)).map_err(|e| e.to_string())?;
        if !after.is_file()
            || (before.dev(), before.ino()) != (after.dev(), after.ino())
            || raw.len() > 16
            || std::str::from_utf8(&raw).map_err(|e| e.to_string())?.trim() != expected
        {
            return Err("owned connector inactive".into());
        }
    }
    let after = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if (meta.dev(), meta.ino()) != (after.dev(), after.ino()) {
        return Err("owned connector changed during admission".into());
    }
    Ok(())
}

impl Device {
    pub fn minimum(&self) -> u32 {
        if self.kind == BacklightKind::Raw && self.max < 99 {
            0
        } else {
            (self.max / 100).max(1)
        }
    }
    pub fn relative(&self, value: u32) -> f64 {
        f64::from(value.saturating_sub(self.minimum())) / f64::from(self.max - self.minimum())
    }
    pub fn absolute(&self, relative: f64) -> u32 {
        self.minimum()
            + (relative.clamp(0.0, 1.0) * f64::from(self.max - self.minimum())).round() as u32
    }
}

pub fn scalar(path: &Path) -> Result<u32, String> {
    let mut raw = Vec::new();
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() {
        return Err("backlight scalar is not regular".into());
    }
    file.by_ref()
        .take(MAX_SCALAR_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let after = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if (before.dev(), before.ino()) != (after.dev(), after.ino()) || !after.is_file() {
        return Err("backlight scalar identity changed".into());
    }
    if raw.len() > MAX_SCALAR_BYTES {
        return Err("backlight scalar exceeds bound".into());
    }
    std::str::from_utf8(&raw)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| "invalid backlight scalar".into())
}

/// No first-device fallback: a capability only covers an associated lit output.
/// Ambiguous legacy ACPI devices remain available to the existing global helper.
pub fn inventory(root: &Path, outputs: &[NativeOutputInfo]) -> Result<Vec<Device>, String> {
    if outputs.len() > 64 {
        return Err("owned output inventory exceeds bound".into());
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut devices = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_DEVICES {
            return Err("backlight inventory exceeds bound".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "nonUTF8 backlight")?;
        let mut matched = BTreeSet::new();
        for target in [entry.path(), entry.path().join("device")] {
            if let Ok(target) = std::fs::canonicalize(target) {
                for owner in outputs {
                    if target
                        .ancestors()
                        .any(|ancestor| ancestor == Path::new(&owner.connector_sysfs))
                    {
                        owner_current(owner)?;
                        matched.insert(owner.name.clone());
                    }
                }
            }
        }
        if matched.len() != 1 {
            continue;
        }
        let output = matched.into_iter().next().unwrap();
        let owners: Vec<_> = outputs
            .iter()
            .filter(|owner| owner.name == output)
            .collect();
        if owners.len() != 1 {
            return Err("ambiguous actual output authority".into());
        }
        let owner = owners[0].clone();
        let kind = BacklightKind::read(&entry.path().join("type"))?;
        // Only actual DRM connector-parent raw interfaces confer per-output
        // authority. Firmware/platform remain explicit legacy global fallback.
        if kind != BacklightKind::Raw {
            continue;
        }
        let identity = std::fs::metadata(entry.path()).map_err(|e| e.to_string())?;
        let max = scalar(&entry.path().join("max_brightness"))?;
        let current = scalar(&entry.path().join("brightness"))?;
        let after = std::fs::metadata(entry.path()).map_err(|e| e.to_string())?;
        if (identity.dev(), identity.ino()) != (after.dev(), after.ino()) {
            return Err("backlight changed during inventory".into());
        }
        let minimum = if kind == BacklightKind::Raw && max < 99 {
            0
        } else {
            (max / 100).max(1)
        };
        if max <= minimum || current > max {
            continue;
        }
        devices.push(Device {
            name,
            output,
            owner: Some(owner),
            kind,
            path: entry.path(),
            max,
            current,
            dev: identity.dev(),
            inode: identity.ino(),
        });
    }
    // Multiple control interfaces for one exact connector are ambiguous.
    let mut counts = std::collections::BTreeMap::new();
    for device in &devices {
        *counts.entry(device.output.clone()).or_insert(0usize) += 1;
    }
    devices.retain(|device| counts[&device.output] == 1);
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(devices)
}

/// Actual post-logind observation; a renamed/replaced device is a failure.
pub fn readback(device: &Device) -> Result<u32, String> {
    if let Some(owner) = &device.owner {
        owner_current(owner)?;
    }
    if BacklightKind::read(&device.path.join("type"))? != device.kind {
        return Err("backlight type changed".into());
    }
    let before = std::fs::metadata(&device.path).map_err(|e| e.to_string())?;
    if (before.dev(), before.ino()) != (device.dev, device.inode) {
        return Err("backlight identity changed".into());
    }
    if scalar(&device.path.join("max_brightness"))? != device.max {
        return Err("backlight range changed".into());
    }
    let actual = scalar(&device.path.join("brightness"))?;
    let after = std::fs::metadata(&device.path).map_err(|e| e.to_string())?;
    if (after.dev(), after.ino()) != (device.dev, device.inode) || actual > device.max {
        return Err("invalid actual backlight readback".into());
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn device(name: &str, output: &str, current: u32) -> Device {
        Device {
            name: name.into(),
            output: output.into(),
            owner: Some(NativeOutputInfo {
                name: output.into(),
                drm_device: 1,
                connector_id: 1,
                connector_sysfs: output.into(),
                connector_device: 1,
                connector_inode: 1,
            }),
            kind: BacklightKind::Firmware,
            path: PathBuf::from(name),
            max: 1000,
            current,
            dev: 1,
            inode: 1,
        }
    }
    fn owner_fixture(path: &Path, name: &str, id: u32) -> NativeOutputInfo {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("connector_id"), id.to_string()).unwrap();
        std::fs::write(path.join("status"), "connected").unwrap();
        std::fs::write(path.join("enabled"), "enabled").unwrap();
        let metadata = std::fs::metadata(path).unwrap();
        NativeOutputInfo {
            name: name.into(),
            drm_device: 1,
            connector_id: id,
            connector_sysfs: std::fs::canonicalize(path)
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            connector_device: metadata.dev(),
            connector_inode: metadata.ino(),
        }
    }
    #[test]
    fn raw_small_floor_follows_actual_type_and_exact_99_boundary() {
        let mut d = device("a", "eDP-1", 0);
        d.kind = BacklightKind::Raw;
        for (max, min) in [(1, 0), (2, 0), (98, 0), (99, 1), (100, 1), (1000, 10)] {
            d.max = max;
            assert_eq!(d.minimum(), min);
            assert_eq!(d.absolute(0.0), min);
        }
        for kind in [BacklightKind::Firmware, BacklightKind::Platform] {
            d.kind = kind;
            d.max = 2;
            assert_eq!(d.minimum(), 1);
        }
    }

    #[test]
    fn backlight_type_missing_malformed_oversized_symlink_and_fifo_refuse() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("type");
        assert!(BacklightKind::read(&path).is_err());
        for value in ["raw\n", "firmware\n", "platform\n"] {
            std::fs::write(&path, value).unwrap();
            assert!(BacklightKind::read(&path).is_ok());
        }
        for value in ["unknown", "raw platform", "", "rawxxxxxxxxxxxxxxxxx"] {
            std::fs::write(&path, value).unwrap();
            assert!(BacklightKind::read(&path).is_err());
        }
        let link = tmp.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(BacklightKind::read(&link).is_err());
        let fifo = tmp.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(BacklightKind::read(&fifo).is_err());
    }

    #[test]
    fn duplicate_interfaces_on_one_owned_connector_withhold_capability() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("backlight");
        std::fs::create_dir(&root).unwrap();
        let owner = owner_fixture(&tmp.path().join("drm/card0/card0-eDP-1"), "eDP-1", 39);
        for name in ["a", "b"] {
            let panel = Path::new(&owner.connector_sysfs).join(name);
            std::fs::create_dir(&panel).unwrap();
            for (attr, value) in [
                ("max_brightness", "1000"),
                ("brightness", "505"),
                ("type", "raw"),
            ] {
                std::fs::write(panel.join(attr), value).unwrap();
            }
            symlink(&panel, root.join(name)).unwrap();
        }
        assert!(inventory(&root, &[owner]).unwrap().is_empty());
    }

    #[test]
    fn scalar_rejects_fifo_symlink_nonregular_and_out_of_range_without_blocking() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("value");
        std::fs::write(&path, "4294967295\n").unwrap();
        assert_eq!(scalar(&path).unwrap(), u32::MAX);
        for value in ["4294967296", "-1", "NaN", "", "1 2"] {
            std::fs::write(&path, value).unwrap();
            assert!(scalar(&path).is_err());
        }
        std::fs::write(&path, [0xff]).unwrap();
        assert!(scalar(&path).is_err());
        std::fs::write(&path, "0".repeat(65)).unwrap();
        assert!(scalar(&path).is_err());
        std::fs::write(&path, "505").unwrap();
        let link = tmp.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(scalar(&link).is_err());
        assert!(scalar(tmp.path()).is_err());
        let fifo = tmp.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(scalar(&fifo).is_err());
    }

    #[test]
    fn oversized_inventory_rejects_whole_capability_even_if_devices_unassociated() {
        let tmp = tempfile::tempdir().unwrap();
        for n in 0..65 {
            std::fs::create_dir(tmp.path().join(n.to_string())).unwrap();
        }
        assert!(inventory(tmp.path(), &[]).is_err());
    }

    #[test]
    fn actual_range_change_removal_and_scalar_replacement_refuse_readback() {
        let tmp = tempfile::tempdir().unwrap();
        let mut d = device("a", "eDP-1", 505);
        d.path = tmp.path().to_owned();
        d.owner = None;
        std::fs::write(d.path.join("type"), "firmware").unwrap();
        let md = std::fs::metadata(&d.path).unwrap();
        d.dev = md.dev();
        d.inode = md.ino();
        std::fs::write(d.path.join("max_brightness"), "1000").unwrap();
        std::fs::write(d.path.join("brightness"), "505").unwrap();
        assert_eq!(readback(&d).unwrap(), 505);
        std::fs::write(d.path.join("max_brightness"), "1010").unwrap();
        assert!(readback(&d).is_err());
        std::fs::write(d.path.join("max_brightness"), "1000").unwrap();
        std::fs::remove_file(d.path.join("brightness")).unwrap();
        assert!(readback(&d).is_err());
    }
    #[test]
    fn selected_gpu_inventory_and_original_connector_replacement_revoke_readback() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("backlight");
        std::fs::create_dir(&root).unwrap();
        let selected_path = tmp.path().join("drm/card0/card0-eDP-1");
        let selected = owner_fixture(&selected_path, "eDP-1", 39);
        let other = owner_fixture(&tmp.path().join("drm/card1/card1-eDP-1"), "eDP-1", 51);
        for (name, owner) in [("a-other", &other), ("z-selected", &selected)] {
            let panel = Path::new(&owner.connector_sysfs).join("panel");
            std::fs::create_dir(&panel).unwrap();
            for (attribute, value) in [
                ("max_brightness", "1000"),
                ("brightness", "505"),
                ("type", "raw"),
            ] {
                std::fs::write(panel.join(attribute), value).unwrap();
            }
            symlink(&panel, root.join(name)).unwrap();
        }
        assert!(inventory(&root, &[]).unwrap().is_empty());
        let devices = inventory(&root, std::slice::from_ref(&selected)).unwrap();
        assert_eq!(devices.len(), 1);
        let device = &devices[0];
        assert_eq!(device.name, "z-selected");
        assert_eq!(device.owner.as_ref(), Some(&selected));
        assert_eq!(readback(device).unwrap(), 505);
        std::fs::write(selected_path.join("connector_id"), "40").unwrap();
        assert!(owner_current(&selected).is_err());
        assert!(readback(device).is_err());
        std::fs::write(selected_path.join("connector_id"), "39").unwrap();
        assert_eq!(readback(device).unwrap(), 505);
        // Same path/name/id after replacement cannot renew the original inode.
        std::fs::rename(&selected_path, selected_path.with_file_name("retired")).unwrap();
        let replacement = owner_fixture(&selected_path, "eDP-1", 39);
        assert_ne!(replacement.connector_inode, selected.connector_inode);
        let panel = selected_path.join("panel");
        std::fs::create_dir(&panel).unwrap();
        for (attribute, value) in [
            ("max_brightness", "1000"),
            ("brightness", "505"),
            ("type", "raw"),
        ] {
            std::fs::write(panel.join(attribute), value).unwrap();
        }
        assert!(owner_current(&selected).is_err());
        assert!(readback(device).is_err());
        assert!(inventory(&root, std::slice::from_ref(&selected)).is_err());
        let fresh = inventory(&root, &[replacement]).unwrap();
        assert_eq!(fresh.len(), 1);
        assert_eq!(readback(&fresh[0]).unwrap(), 505);
        // A newly supplied owner is a separate raw observation, not restoration authority.
    }
}
