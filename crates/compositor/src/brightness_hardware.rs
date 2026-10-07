//! One bounded off-frame validator. It observes real sysfs only and never writes.
//! Original backend generations and supervised connection lifetimes remain
//! compositor-owned; a successful parser alone never grants hardware authority.
use crate::{
    brightness_journal::{self as journal, Binding, FreshReadback},
    control::{BrightnessRequest, OwnedBrightnessRequest},
};
use roost_shell_control::NativeOutputInfo;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
    Arc, Mutex,
};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeAuthority {
    pub generation: u64,
    pub outputs: Vec<NativeOutputInfo>,
}
pub struct Validated {
    pub original: OwnedBrightnessRequest,
    pub native: NativeAuthority,
    pub facts: Result<Vec<FreshReadback>, journal::Error>,
}
pub struct Worker {
    native: Arc<Mutex<Option<NativeAuthority>>>,
    send: SyncSender<OwnedBrightnessRequest>,
    receive: Receiver<Validated>,
    alive: Arc<AtomicBool>,
}
impl Worker {
    pub fn start() -> Self {
        let native = Arc::new(Mutex::new(None));
        let (send, requests) = mpsc::sync_channel::<OwnedBrightnessRequest>(8);
        let (responses, receive) = mpsc::sync_channel(8);
        let state = native.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = alive.clone();
        let spawned = std::thread::Builder::new()
            .name("brightness-readback".into())
            .spawn(move || {
                let _exit = Exit {
                    native: state.clone(),
                    alive: worker_alive,
                };
                while let Ok(original) = requests.recv() {
                    let current = state.lock().ok().and_then(|v| v.clone());
                    let native = current.unwrap_or(NativeAuthority {
                        generation: 0,
                        outputs: Vec::new(),
                    });
                    let facts = validate(&original.request, &native, &state);
                    // Saturation cannot grow memory or block a display thread;
                    // missing validation completion never grants helper admission.
                    if responses
                        .try_send(Validated {
                            original,
                            native,
                            facts,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        if spawned.is_err() {
            alive.store(false, Ordering::Release);
        }
        Self {
            native,
            send,
            receive,
            alive,
        }
    }
    /// Runtime supplies this only from actual accepted native backend ownership.
    /// None or any changed generation revokes all outstanding original facts.
    pub fn publish(&self, native: Option<NativeAuthority>) {
        if let Ok(mut state) = self.native.lock() {
            *state = native.filter(|v| {
                self.alive.load(Ordering::Acquire)
                    && v.generation != 0
                    && v.outputs.len() <= journal::MAX_PANELS
            });
        }
    }
    pub fn authority(&self) -> Option<NativeAuthority> {
        if !self.alive.load(Ordering::Acquire) {
            return None;
        }
        self.native.lock().ok()?.clone()
    }
    pub fn submit(&self, original: OwnedBrightnessRequest) -> Result<(), journal::Error> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(journal::Error::Binding);
        }
        self.send
            .try_send(original)
            .map_err(|_| journal::Error::Busy)
    }
    pub fn poll(&self) -> Option<Validated> {
        self.receive.try_recv().ok()
    }
}
struct Exit {
    native: Arc<Mutex<Option<NativeAuthority>>>,
    alive: Arc<AtomicBool>,
}
impl Drop for Exit {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(mut native) = self.native.lock() {
            *native = None;
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Ok(mut state) = self.native.lock() {
            *state = None;
        }
    }
}
fn requested_bindings(request: &BrightnessRequest) -> Vec<Binding> {
    match request {
        BrightnessRequest::Init { readings, .. } => {
            readings.iter().map(|v| v.binding.clone().into()).collect()
        }
        BrightnessRequest::Begin { targets, .. } => {
            targets.iter().map(|v| v.binding.clone().into()).collect()
        }
        BrightnessRequest::Complete { observations, .. } => observations
            .iter()
            .map(|v| v.binding.clone().into())
            .collect(),
    }
}
fn native_matches(native: &NativeAuthority, state: &Arc<Mutex<Option<NativeAuthority>>>) -> bool {
    state
        .lock()
        .ok()
        .is_some_and(|v| v.as_ref() == Some(native))
}
fn validate(
    request: &BrightnessRequest,
    native: &NativeAuthority,
    state: &Arc<Mutex<Option<NativeAuthority>>>,
) -> Result<Vec<FreshReadback>, journal::Error> {
    validate_at(
        request,
        native,
        state,
        std::path::Path::new("/sys/class/backlight"),
        |_| {},
    )
}
fn validate_at(
    request: &BrightnessRequest,
    native: &NativeAuthority,
    state: &Arc<Mutex<Option<NativeAuthority>>>,
    root: &std::path::Path,
    mut observed: impl FnMut(usize),
) -> Result<Vec<FreshReadback>, journal::Error> {
    if native.generation == 0 || !native_matches(native, state) {
        return Err(journal::Error::Binding);
    }
    let bindings = requested_bindings(request);
    if bindings.len() > journal::MAX_PANELS {
        return Err(journal::Error::Bounds);
    }
    let checked = Instant::now();
    let devices =
        roost_backlight::inventory(root, &native.outputs).map_err(|_| journal::Error::Binding)?;
    // First import represents ALL actual owned raw panel interfaces. A
    // shell-selected subset cannot establish a whole-panel measured baseline.
    if matches!(request, BrightnessRequest::Init { .. }) && devices.len() != bindings.len() {
        return Err(journal::Error::Incomplete);
    }
    let mut facts = Vec::with_capacity(bindings.len());
    for (index, binding) in bindings.iter().enumerate() {
        if binding.ownership_generation != native.generation || bindings[..index].contains(binding)
        {
            return Err(journal::Error::Binding);
        }
        let device = devices
            .iter()
            .find(|v| {
                v.name == binding.backlight
                    && v.owner.as_ref() == Some(&binding.output)
                    && v.dev == binding.device
                    && v.inode == binding.inode
                    && v.minimum() == binding.minimum
                    && v.max == binding.maximum
            })
            .ok_or(journal::Error::Binding)?;
        let actual = roost_backlight::readback(device).map_err(|_| journal::Error::Binding)?;
        facts.push(FreshReadback {
            binding: binding.clone(),
            actual,
            checked_at: checked,
        });
        observed(index);
    }
    // Revalidate the ENTIRE original device/binding/range set after the last
    // observation. This is a bracketed observation, not atomic hardware state.
    let final_devices =
        roost_backlight::inventory(root, &native.outputs).map_err(|_| journal::Error::Binding)?;
    if matches!(request, BrightnessRequest::Init { .. }) && final_devices.len() != devices.len() {
        return Err(journal::Error::Incomplete);
    }
    for fact in &facts {
        let binding = &fact.binding;
        let device = final_devices
            .iter()
            .find(|v| {
                v.name == binding.backlight
                    && v.owner.as_ref() == Some(&binding.output)
                    && v.dev == binding.device
                    && v.inode == binding.inode
                    && v.minimum() == binding.minimum
                    && v.max == binding.maximum
            })
            .ok_or(journal::Error::Binding)?;
        if roost_backlight::readback(device).map_err(|_| journal::Error::Binding)? != fact.actual {
            return Err(journal::Error::Stale);
        }
    }
    if !native_matches(native, state) || checked.elapsed() > std::time::Duration::from_secs(2) {
        return Err(journal::Error::Stale);
    }
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_generation_never_reads_host_sysfs() {
        let native = NativeAuthority {
            generation: 0,
            outputs: Vec::new(),
        };
        let state = Arc::new(Mutex::new(None));
        let request = BrightnessRequest::Init {
            request: 1,
            readings: Vec::new(),
            idle: 0.3,
        };
        assert_eq!(
            validate(&request, &native, &state),
            Err(journal::Error::Binding)
        );
    }
    #[test]
    fn generation_replacement_and_none_revoke_original_facts() {
        let original = NativeAuthority {
            generation: 1,
            outputs: Vec::new(),
        };
        let state = Arc::new(Mutex::new(Some(original.clone())));
        assert!(native_matches(&original, &state));
        *state.lock().unwrap() = Some(NativeAuthority {
            generation: 2,
            outputs: Vec::new(),
        });
        assert!(!native_matches(&original, &state));
        *state.lock().unwrap() = None;
        assert!(!native_matches(&original, &state));
    }
    fn fixture() -> (tempfile::TempDir, NativeAuthority, BrightnessRequest) {
        use std::os::unix::fs::{symlink, MetadataExt};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("backlight");
        std::fs::create_dir(&root).unwrap();
        let mut outputs = Vec::new();
        for (id, name) in [(1, "eDP-1"), (2, "eDP-2")] {
            let connector = dir.path().join(name);
            std::fs::create_dir(&connector).unwrap();
            for (attr, value) in [
                ("connector_id", id.to_string()),
                ("status", "connected".into()),
                ("enabled", "enabled".into()),
            ] {
                std::fs::write(connector.join(attr), value).unwrap();
            }
            let panel = connector.join("panel");
            std::fs::create_dir(&panel).unwrap();
            for (attr, value) in [
                ("max_brightness", "1000"),
                ("brightness", "300"),
                ("type", "raw"),
            ] {
                std::fs::write(panel.join(attr), value).unwrap();
            }
            symlink(&panel, root.join(name)).unwrap();
            let metadata = connector.metadata().unwrap();
            outputs.push(NativeOutputInfo {
                name: name.into(),
                drm_device: 1,
                connector_id: id,
                connector_sysfs: connector.to_str().unwrap().into(),
                connector_device: metadata.dev(),
                connector_inode: metadata.ino(),
            });
        }
        let devices = roost_backlight::inventory(&root, &outputs).unwrap();
        let targets = devices
            .iter()
            .map(|v| roost_shell_control::BrightnessTarget {
                binding: roost_shell_control::BrightnessBinding {
                    output: v.owner.clone().unwrap(),
                    ownership_generation: 1,
                    backlight: v.name.clone(),
                    device: v.dev,
                    inode: v.inode,
                    minimum: v.minimum(),
                    maximum: v.max,
                },
                effective: 300,
                user: None,
                ratio: None,
            })
            .collect();
        (
            dir,
            NativeAuthority {
                generation: 1,
                outputs,
            },
            BrightnessRequest::Begin {
                request: 1,
                source: roost_shell_control::BrightnessSource::Automatic,
                targets,
            },
        )
    }
    #[test]
    fn initial_import_requires_entire_actual_panel_inventory_before_and_after_reads() {
        let (dir, native, request) = fixture();
        let BrightnessRequest::Begin { targets, .. } = request else {
            panic!("fixture targets");
        };
        let readings = targets
            .into_iter()
            .map(|target| roost_shell_control::BrightnessReading {
                binding: target.binding,
                user: 300,
                user_known: false,
                measured_candidate: None,
                measured_restoration: None,
                applied: 300,
                ratio: 1.0,
            })
            .collect::<Vec<_>>();
        let state = Arc::new(Mutex::new(Some(native.clone())));
        let root = dir.path().join("backlight");
        let partial = BrightnessRequest::Init {
            request: 1,
            readings: readings[..1].to_vec(),
            idle: 0.3,
        };
        assert!(matches!(
            validate_at(&partial, &native, &state, &root, |_| {}),
            Err(journal::Error::Incomplete)
        ));
        let full = BrightnessRequest::Init {
            request: 2,
            readings,
            idle: 0.3,
        };
        assert!(validate_at(&full, &native, &state, &root, |_| {}).is_ok());
        assert!(validate_at(&full, &native, &state, &root, |index| {
            if index == 1 {
                std::fs::write(dir.path().join("eDP-1/enabled"), "disabled").unwrap();
            }
        })
        .is_err());
    }
    #[test]
    fn entire_original_set_is_rechecked_after_later_observations() {
        for mutation in 0..3 {
            let (dir, native, request) = fixture();
            let root = dir.path().join("backlight");
            let state = Arc::new(Mutex::new(Some(native.clone())));
            assert!(validate_at(&request, &native, &state, &root, |_| {}).is_ok());
            let result = validate_at(&request, &native, &state, &root, |index| {
                if index != 1 {
                    return;
                }
                let early = dir.path().join("eDP-1");
                match mutation {
                    0 => std::fs::write(early.join("panel/max_brightness"), "999").unwrap(),
                    1 => {
                        std::fs::rename(early.join("panel"), early.join("old-panel")).unwrap();
                        std::fs::create_dir(early.join("panel")).unwrap();
                        for (attr, value) in [
                            ("max_brightness", "1000"),
                            ("brightness", "300"),
                            ("type", "raw"),
                        ] {
                            std::fs::write(early.join("panel").join(attr), value).unwrap();
                        }
                    }
                    _ => std::fs::write(early.join("enabled"), "disabled").unwrap(),
                }
            });
            assert!(
                result.is_err(),
                "early original binding changed after later observation"
            );
        }
    }
    #[test]
    fn response_saturation_exit_revokes_native_authority() {
        let worker = Worker::start();
        // generation=None rejects before filesystem inventory: exercise the
        // actual bounded request/response channels without host hardware.
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        let mut sent = 0;
        while worker.alive.load(Ordering::Acquire) && Instant::now() < deadline {
            let original = OwnedBrightnessRequest {
                authority: journal::Authority {
                    child_generation: 1,
                    connection_generation: 1,
                },
                request: BrightnessRequest::Init {
                    request: sent + 1,
                    readings: Vec::new(),
                    idle: 0.3,
                },
            };
            if worker.submit(original).is_ok() {
                sent += 1;
            }
            std::thread::yield_now();
        }
        assert!(sent >= 9, "actual response queue must fill before exit");
        assert!(!worker.alive.load(Ordering::Acquire));
        worker.publish(Some(NativeAuthority {
            generation: 1,
            outputs: Vec::new(),
        }));
        assert!(worker.authority().is_none());
        assert!(worker.native.lock().unwrap().is_none());
        assert!(worker
            .submit(OwnedBrightnessRequest {
                authority: journal::Authority {
                    child_generation: 1,
                    connection_generation: 1
                },
                request: BrightnessRequest::Init {
                    request: 99,
                    readings: Vec::new(),
                    idle: 0.3
                },
            })
            .is_err());
    }
}
