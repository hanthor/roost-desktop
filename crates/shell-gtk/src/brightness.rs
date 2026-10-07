//! GNOME 51 brightness policy for associated, compositor-tracked backlights.
//! A sysfs fixture tests this policy; it is not evidence of physical hardware.

use roost_shell_control::NativeOutputInfo;
use std::collections::{BTreeMap, BTreeSet};
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
    pub(crate) fn read(path: &Path) -> Result<Self, String> {
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

fn owner_current(owner: &NativeOutputInfo) -> Result<(), String> {
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

pub(crate) fn scalar(path: &Path) -> Result<u32, String> {
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

/// Explicit controlled fixture ONLY. Caller must also have an overridden
/// backlight root. This never supplies native hardware evidence.
pub fn controlled_outputs(path: &Path) -> Result<Vec<NativeOutputInfo>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| e.to_string())?;
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.uid() != unsafe { libc::geteuid() } {
        return Err("controlled manifest owner/type mismatch".into());
    }
    let mut raw = Vec::new();
    file.by_ref()
        .take(65537)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let after = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if raw.len() > 65536
        || !after.is_file()
        || (before.dev(), before.ino()) != (after.dev(), after.ino())
    {
        return Err("controlled manifest identity/bound mismatch".into());
    }
    let outputs: Vec<NativeOutputInfo> = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    if outputs.len() > 64 {
        return Err("controlled manifest count exceeds bound".into());
    }
    for output in &outputs {
        if output.name.len() > 512 || output.connector_sysfs.len() > 512 {
            return Err("controlled manifest field exceeds bound".into());
        }
        owner_current(output)?;
    }
    Ok(outputs)
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

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub dimming: bool,
    pub idle: f64,
    pub auto: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            dimming: false,
            idle: 0.3,
            auto: -1.0,
        }
    }
}
impl Policy {
    pub fn level(self, user: f64) -> Result<f64, String> {
        if !user.is_finite() || !self.idle.is_finite() || !self.auto.is_finite() {
            return Err("non-finite brightness policy".into());
        }
        let target = if self.auto >= 0.0 {
            (self.auto + user - 0.5).clamp(0.0, 1.0)
        } else {
            user.clamp(0.0, 1.0)
        };
        Ok(target.min(if self.dimming {
            self.idle.clamp(0.0, 1.0)
        } else {
            1.0
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Reading {
    device: Device,
    user: u32,
    applied: u32,
}

#[derive(Default)]
pub struct Model {
    pub policy: Policy,
    readings: BTreeMap<String, Reading>,
    factors: BTreeMap<String, f64>,
}
impl Model {
    pub fn available(&self) -> bool {
        !self.readings.is_empty()
    }
    pub fn has_output(&self, output: Option<&str>) -> bool {
        self.readings
            .values()
            .any(|r| output.is_none_or(|o| r.device.output == o))
    }
    /// Refresh actual readings. External hardware changes end dimming, as GNOME
    /// does; policy writes retain the user baseline through their actual receipt.
    pub fn refresh(&mut self, devices: Vec<Device>) -> bool {
        let previous = self.readings.clone();
        let mut next = BTreeMap::new();
        for device in devices {
            let old = self.readings.remove(&device.name).filter(|r| {
                r.device.path == device.path
                    && r.device.output == device.output
                    && r.device.owner == device.owner
                    && r.device.kind == device.kind
                    && r.device.max == device.max
                    && r.device.dev == device.dev
                    && r.device.inode == device.inode
            });
            let reading = match old {
                Some(mut reading) if reading.applied == device.current => {
                    reading.device = device;
                    reading
                }
                Some(_) => {
                    self.policy.dimming = false;
                    Reading {
                        user: device.current,
                        applied: device.current,
                        device,
                    }
                }
                None => Reading {
                    user: device.current,
                    applied: device.current,
                    device,
                },
            };
            next.insert(reading.device.name.clone(), reading);
        }
        let changed = previous != next;
        self.readings = next;
        self.factors
            .retain(|name, _| self.readings.contains_key(name));
        for name in self.readings.keys() {
            self.factors.entry(name.clone()).or_insert(1.0);
        }
        if changed {
            self.sync_factors();
        }
        changed
    }
    /// GNOME retains known monitor ratios when every scale is near zero.
    /// Call after a complete user transaction, never after a partial batch.
    pub fn sync_factors(&mut self) {
        let max = self.user_level(None).unwrap_or(0.0);
        if max > 0.01 {
            for r in self.readings.values() {
                self.factors
                    .insert(r.device.name.clone(), r.device.relative(r.user) / max);
            }
        }
    }
    pub fn targets(&self) -> Result<Vec<(Device, u32)>, String> {
        self.readings
            .values()
            .map(|r| {
                self.policy
                    .level(r.device.relative(r.user))
                    .map(|level| (r.device.clone(), r.device.absolute(level)))
            })
            .collect()
    }
    /// User intent and policy-adjusted physical target are distinct receipts.
    pub fn user_targets(
        &self,
        output: Option<&str>,
        level: f64,
    ) -> Result<Vec<(Device, u32, Option<u32>)>, String> {
        if !level.is_finite() {
            return Err("non-finite user brightness".into());
        }
        self.readings
            .values()
            .filter(|r| output.is_none_or(|o| r.device.output == o))
            .map(|r| {
                let ratio = if output.is_some() {
                    1.0
                } else {
                    *self.factors.get(&r.device.name).unwrap_or(&1.0)
                };
                let desired = (level * ratio).clamp(0.0, 1.0);
                self.policy.level(desired).map(|effective| {
                    (
                        r.device.clone(),
                        r.device.absolute(effective),
                        Some(r.device.absolute(desired)),
                    )
                })
            })
            .collect()
    }
    pub fn step_size(&self, output: Option<&str>) -> Option<f64> {
        self.readings
            .values()
            .filter(|r| output.is_none_or(|o| r.device.output == o))
            .map(|r| r.device.max - r.device.minimum())
            .max()
            .map(|n| 1.0 / f64::from(n.min(20)))
    }
    pub fn user_level(&self, output: Option<&str>) -> Option<f64> {
        self.readings
            .values()
            .filter(|r| output.is_none_or(|o| r.device.output == o))
            .map(|r| r.device.relative(r.user))
            .reduce(f64::max)
    }
    /// Failed transport still has a separate actual observation. Preserve user
    /// intent rather than misclassifying a partial policy write as a user change.
    pub fn observe_failure(&mut self, device: &Device, actual: u32) -> Result<(), String> {
        let r = self
            .readings
            .get_mut(&device.name)
            .ok_or("backlight removed")?;
        if r.device.path != device.path
            || r.device.output != device.output
            || r.device.owner != device.owner
            || r.device.kind != device.kind
            || r.device.max != device.max
            || r.device.dev != device.dev
            || r.device.inode != device.inode
            || actual > device.max
        {
            return Err("failed backlight observation identity mismatch".into());
        }
        r.applied = actual;
        r.device.current = actual;
        Ok(())
    }
    /// Called only after successful logind completion and exact sysfs readback.
    /// A failed/removed/replaced device never becomes an acknowledged user value.
    pub fn acknowledge(
        &mut self,
        device: &Device,
        target: u32,
        observed: u32,
        user: Option<u32>,
    ) -> Result<f64, String> {
        let r = self
            .readings
            .get_mut(&device.name)
            .ok_or("backlight removed")?;
        if r.device.path != device.path
            || r.device.output != device.output
            || r.device.owner != device.owner
            || r.device.kind != device.kind
            || r.device.max != device.max
            || r.device.dev != device.dev
            || r.device.inode != device.inode
            || observed > r.device.max
            || user.is_some_and(|value| value < r.device.minimum() || value > r.device.max)
        {
            return Err("backlight readback or identity mismatch".into());
        }
        r.device.current = observed;
        r.applied = observed;
        if target != observed {
            return Err("backlight readback mismatch".into());
        }
        if let Some(user) = user {
            r.user = user;
        }
        Ok(r.device.relative(observed))
    }
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
    fn same_connector_name_on_other_gpu_never_acquires_or_receives_target() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("backlight");
        std::fs::create_dir(&root).unwrap();
        let selected = owner_fixture(&tmp.path().join("drm/card0/card0-eDP-1"), "eDP-1", 39);
        let other = owner_fixture(&tmp.path().join("drm/card1/card1-eDP-1"), "eDP-1", 51);
        for (name, owner) in [("a-other", &other), ("z-selected", &selected)] {
            let panel = Path::new(&owner.connector_sysfs).join("panel");
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
        assert!(inventory(&root, &[]).unwrap().is_empty());
        let selected_devices = inventory(&root, std::slice::from_ref(&selected)).unwrap();
        assert_eq!(selected_devices.len(), 1);
        assert_eq!(selected_devices[0].name, "z-selected");
        let mut model = Model::default();
        model.refresh(selected_devices.clone());
        assert_eq!(
            model.user_targets(Some("eDP-1"), 0.3).unwrap()[0].0.name,
            "z-selected"
        );
        std::fs::write(
            Path::new(&selected.connector_sysfs).join("enabled"),
            "disabled",
        )
        .unwrap();
        assert!(readback(&selected_devices[0]).is_err());
        assert!(inventory(&root, std::slice::from_ref(&selected)).is_err());
        std::fs::write(
            Path::new(&selected.connector_sysfs).join("enabled"),
            "enabled",
        )
        .unwrap();
        std::fs::write(
            Path::new(&selected.connector_sysfs).join("connector_id"),
            "40",
        )
        .unwrap();
        assert!(readback(&selected_devices[0]).is_err());
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
    fn live_idle_policy_changes_physical_target_without_replacing_user_intent() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 700)]);
        m.policy.dimming = true;
        let (d, n) = m.targets().unwrap()[0].clone();
        m.acknowledge(&d, n, n, None).unwrap();
        m.policy.idle = 0.2;
        let (d, n) = m.targets().unwrap()[0].clone();
        assert_eq!(n, 208);
        m.acknowledge(&d, n, n, None).unwrap();
        m.refresh(vec![device("a", "eDP-1", 208)]);
        m.policy.dimming = false;
        assert_eq!(m.targets().unwrap()[0].1, 700);
        // A real external change cancels dimming BEFORE deriving an auto request.
        m.policy.dimming = true;
        m.refresh(vec![device("a", "eDP-1", 505)]);
        assert!(!m.policy.dimming);
        m.policy.auto = 0.8;
        assert_eq!(m.targets().unwrap()[0].1, 802);
    }
    #[test]
    fn global_zero_preserves_known_monitor_ratios_across_complete_transactions() {
        let mut m = Model::default();
        m.refresh(vec![
            device("a", "eDP-1", 1000),
            device("b", "HDMI-A-1", 505),
        ]);
        for (d, n, user) in m.user_targets(None, 0.0).unwrap() {
            m.acknowledge(&d, n, n, user).unwrap();
        }
        m.sync_factors();
        m.refresh(vec![device("a", "eDP-1", 10), device("b", "HDMI-A-1", 10)]);
        assert_eq!(
            m.user_targets(None, 0.6)
                .unwrap()
                .iter()
                .map(|(_, n, _)| *n)
                .collect::<Vec<_>>(),
            vec![604, 307]
        );
    }
    #[test]
    fn user_intent_while_dimmed_or_auto_is_restored_after_actual_effective_receipt() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 505)]);
        m.policy.dimming = true;
        let (d, n, user) = m.user_targets(None, 0.8).unwrap()[0].clone();
        assert_eq!((n, user), (307, Some(802)));
        assert_eq!(m.acknowledge(&d, n, 307, user).unwrap(), 0.3);
        m.refresh(vec![device("a", "eDP-1", 307)]);
        m.policy.dimming = false;
        assert_eq!(m.targets().unwrap()[0].1, 802);
        m.policy.auto = 0.7;
        let (d, n, user) = m.user_targets(None, 0.6).unwrap()[0].clone();
        assert_eq!((n, user), (802, Some(604)));
        m.acknowledge(&d, n, n, user).unwrap();
        m.policy.auto = -1.0;
        assert_eq!(m.targets().unwrap()[0].1, 604);
    }
    #[test]
    fn few_step_hardware_uses_real_range_not_twenty_inert_steps() {
        let mut d = device("a", "eDP-1", 1);
        d.max = 2;
        d.kind = BacklightKind::Raw;
        let mut m = Model::default();
        m.refresh(vec![d]);
        assert_eq!(m.step_size(None), Some(0.5));
        assert_eq!(m.user_targets(None, 1.0).unwrap()[0].1, 2);
    }
    #[test]
    fn dim_restore_keeps_actual_user_value_and_floor_relative_mapping() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 700)]);
        m.policy.dimming = true;
        let (d, target) = m.targets().unwrap().pop().unwrap();
        assert_eq!(target, 307);
        m.acknowledge(&d, target, 307, None).unwrap();
        m.refresh(vec![device("a", "eDP-1", 307)]);
        m.policy.dimming = false;
        assert_eq!(m.targets().unwrap()[0].1, 700);
    }
    #[test]
    fn auto_bias_disable_and_dimming_clip_preserve_user_baseline() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 505)]);
        m.policy.auto = 0.8;
        assert_eq!(m.targets().unwrap()[0].1, 802);
        m.policy.dimming = true;
        assert_eq!(m.targets().unwrap()[0].1, 307);
        m.policy.auto = -1.0;
        m.policy.dimming = false;
        assert_eq!(m.targets().unwrap()[0].1, 505);
        for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            m.policy.auto = n;
            assert!(m.targets().is_err());
        }
    }
    #[test]
    fn rejected_readback_and_removed_device_never_acknowledge_user_level() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 505)]);
        let d = m.targets().unwrap()[0].0.clone();
        assert!(m.acknowledge(&d, 700, 699, Some(700)).is_err());
        assert_eq!(m.user_level(None), Some(0.5));
        m.refresh(vec![device("a", "eDP-1", 699)]);
        assert_eq!(m.targets().unwrap()[0].1, 505);
        m.observe_failure(&d, 810).unwrap();
        m.refresh(vec![device("a", "eDP-1", 810)]);
        assert_eq!(m.targets().unwrap()[0].1, 505);
        m.refresh(vec![]);
        assert!(!m.available());
        assert!(m.acknowledge(&d, 700, 700, Some(700)).is_err());
    }
    #[test]
    fn reordered_outputs_and_pointer_target_never_write_other_monitor() {
        let mut m = Model::default();
        m.refresh(vec![
            device("b", "HDMI-A-1", 400),
            device("a", "eDP-1", 700),
        ]);
        assert_eq!(
            m.user_targets(Some("eDP-1"), 0.3)
                .unwrap()
                .iter()
                .map(|(d, n, _)| (&d.name, *n))
                .collect::<Vec<_>>(),
            vec![(&"a".to_owned(), 307)]
        );
        m.refresh(vec![
            device("a", "eDP-1", 700),
            device("b", "HDMI-A-1", 400),
        ]);
        assert!(m.user_targets(Some("DP-3"), 0.5).unwrap().is_empty());
        assert_eq!(m.user_level(Some("HDMI-A-1")), Some(390.0 / 990.0));
    }
    #[test]
    fn replacement_same_name_and_path_requires_actual_new_identity() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 700)]);
        let d = m.targets().unwrap()[0].0.clone();
        let mut replaced = d.clone();
        replaced.inode = 2;
        m.refresh(vec![replaced]);
        assert!(m.acknowledge(&d, 700, 700, Some(700)).is_err());
    }
    #[test]
    fn global_scale_preserves_actual_monitor_ratios() {
        let mut m = Model::default();
        m.refresh(vec![
            device("a", "eDP-1", 1000),
            device("b", "HDMI-A-1", 505),
        ]);
        let targets = m.user_targets(None, 0.6).unwrap();
        assert_eq!(
            targets.iter().map(|(_, n, _)| *n).collect::<Vec<_>>(),
            vec![604, 307]
        );
    }
    #[test]
    fn inventory_requires_actual_connector_and_valid_bounded_readings() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("backlight");
        std::fs::create_dir(&root).unwrap();
        let actual = tmp.path().join("drm/card0/card0-eDP-1/panel");
        std::fs::create_dir_all(&actual).unwrap();
        std::fs::write(actual.join("max_brightness"), "1000").unwrap();
        std::fs::write(actual.join("brightness"), "505").unwrap();
        symlink(&actual, root.join("panel")).unwrap();
        std::fs::write(actual.join("type"), "raw").unwrap();
        let owner = owner_fixture(actual.parent().unwrap(), "eDP-1", 39);
        assert!(inventory(&root, &[]).unwrap().is_empty());
        assert_eq!(
            inventory(&root, std::slice::from_ref(&owner)).unwrap()[0].current,
            505
        );
        std::fs::write(actual.join("max_brightness"), "0").unwrap();
        assert!(inventory(&root, std::slice::from_ref(&owner))
            .unwrap()
            .is_empty());
        std::fs::write(actual.join("max_brightness"), "1".repeat(65)).unwrap();
        assert!(inventory(&root, std::slice::from_ref(&owner)).is_err());
        std::fs::remove_file(actual.join("max_brightness")).unwrap();
        assert!(inventory(&root, std::slice::from_ref(&owner)).is_err());
    }
    #[test]
    fn newly_associated_monitor_preserves_policy_and_requires_real_application() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 700)]);
        m.policy.dimming = true;
        let (d, n) = m.targets().unwrap()[0].clone();
        m.acknowledge(&d, n, n, None).unwrap();
        m.refresh(vec![
            device("a", "eDP-1", 307),
            device("b", "HDMI-A-1", 800),
        ]);
        assert!(m.policy.dimming);
        assert_eq!(
            m.targets()
                .unwrap()
                .iter()
                .map(|(_, n)| *n)
                .collect::<Vec<_>>(),
            vec![307, 307]
        );
    }
    #[test]
    fn model_policies_and_failed_readbacks_are_not_user_updates() {
        let mut m = Model::default();
        m.refresh(vec![device("a", "eDP-1", 700)]);
        m.policy.dimming = true;
        let (d, n) = m.targets().unwrap()[0].clone();
        m.acknowledge(&d, n, n, None).unwrap();
        assert_eq!(m.user_level(None), Some(690.0 / 990.0));
        m.refresh(vec![device("a", "eDP-1", 500)]);
        assert!(!m.policy.dimming);
        assert_eq!(m.user_level(None), Some(490.0 / 990.0));
    }
}
