//! GNOME51 brightness policy; hardware reads use the unchanged shared implementation.
use roost_backlight::owner_current;
pub use roost_backlight::{inventory, readback, BacklightKind, Device};
use roost_shell_control::NativeOutputInfo;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

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

#[derive(Clone, Default)]
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
    /// Import original compositor intent without a hardware write. Current
    /// effective readings stay distinct from user intent and retired bindings
    /// never match a new panel merely by connector text.
    pub fn import_journal(
        &mut self,
        state: &roost_shell_control::BrightnessJournalSnapshot,
        devices: Vec<Device>,
    ) -> Result<(), String> {
        let generation = state
            .native_generation
            .ok_or("native brightness unavailable")?;
        if state.readings.len() != devices.len() {
            return Err("original brightness panel inventory changed".into());
        }
        let mut readings = BTreeMap::new();
        let mut factors = BTreeMap::new();
        for device in devices {
            let row = state
                .readings
                .iter()
                .find(|r| {
                    r.binding.ownership_generation == generation
                        && Some(&r.binding.output) == device.owner.as_ref()
                        && r.binding.backlight == device.name
                        && r.binding.device == device.dev
                        && r.binding.inode == device.inode
                        && r.binding.minimum == device.minimum()
                        && r.binding.maximum == device.max
                })
                .ok_or("original brightness import binding missing")?;
            if row.user < device.minimum()
                || row.user > device.max
                || row.applied > device.max
                || !row.ratio.is_finite()
                || !(0.0..=1.0).contains(&row.ratio)
            {
                return Err("invalid original brightness import".into());
            }
            // UI/model arithmetic uses the original admitted restoration cap,
            // while the compositor alone retains whether it is User intent.
            let user = if row.user_known {
                row.user
            } else if let Some(measured) = row.measured_restoration.filter(|v| {
                state
                    .provider
                    .as_ref()
                    .is_some_and(|p| p.epoch == v.provider_epoch)
            }) {
                measured.level
            } else {
                row.applied.max(device.minimum())
            };
            if user > device.max {
                return Err("invalid measured brightness import".into());
            }
            factors.insert(device.name.clone(), row.ratio);
            readings.insert(
                device.name.clone(),
                Reading {
                    device,
                    user,
                    applied: row.applied,
                },
            );
        }
        self.readings = readings;
        self.factors = factors;
        self.policy = Policy {
            dimming: state.policy.dimming.unwrap_or(false),
            auto: state.policy.automatic.unwrap_or(-1.0),
            idle: state.policy.idle,
        };
        Ok(())
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
    /// Stage every original helper receipt before committing ANY User intent.
    /// Actual partial observations are retained separately by observe_failure.
    pub fn acknowledge_batch(
        &mut self,
        receipts: &[(Device, u32, u32, Option<u32>)],
    ) -> Result<f64, String> {
        if receipts.is_empty() || receipts.len() > 64 {
            return Err("invalid backlight receipt count".into());
        }
        let mut staged = self.clone();
        let mut names = std::collections::BTreeSet::new();
        let mut level = 0.0f64;
        for (device, target, actual, user) in receipts {
            if !names.insert(&device.name) {
                return Err("duplicate backlight receipt".into());
            }
            level = level.max(staged.acknowledge(device, *target, *actual, *user)?);
        }
        *self = staged;
        Ok(level)
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
    use std::path::PathBuf;
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
    #[test]
    fn partial_batch_never_commits_first_successful_user_receipt() {
        let mut model = Model::default();
        let a = device("a", "eDP-1", 700);
        let b = device("b", "eDP-2", 800);
        model.refresh(vec![a.clone(), b.clone()]);
        model.observe_failure(&a, 200).unwrap();
        model.observe_failure(&b, 300).unwrap();
        let receipts = vec![
            (a.clone(), 200, 200, Some(900)),
            (b.clone(), 200, 300, Some(900)),
        ];
        assert!(model.acknowledge_batch(&receipts).is_err());
        assert_eq!(model.readings["a"].user, 700);
        assert_eq!(model.readings["b"].user, 800);
        assert_eq!(model.readings["a"].applied, 200);
        assert_eq!(model.readings["b"].applied, 300);
        let receipts = vec![(a, 200, 200, Some(900)), (b, 200, 200, Some(900))];
        model.acknowledge_batch(&receipts).unwrap();
        assert_eq!(model.readings["a"].user, 900);
        assert_eq!(model.readings["b"].user, 900);
    }
}
