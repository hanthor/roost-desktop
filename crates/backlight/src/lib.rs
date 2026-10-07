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
