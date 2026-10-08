//! The GNOME 51 brightness contract. Positive capability requires an actual
//! associated backlight; synthetic test roots are never native qualification.
use crate::brightness::{self, Device, Model, Policy};
use crate::brightness_journal_client::{self as journal, Command as JournalCommand, Permit};
use gio::prelude::*;
use glib::variant::ToVariant;
use roost_shell_control::{
    BrightnessBinding, BrightnessObservation, BrightnessPolicyUpdate, BrightnessReading,
    BrightnessSource, BrightnessTarget,
};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

const NAME: &str = "org.gnome.Shell.Brightness";
const PATH: &str = "/org/gnome/Shell/Brightness";
const XML: &str = r#"<node><interface name="org.gnome.Shell.Brightness">
<method name="SetDimming"><arg type="b" direction="in" name="enable"/></method>
<method name="SetAutoBrightnessTarget"><arg type="d" direction="in" name="target"/></method>
<property name="HasBrightnessControl" type="b" access="read"/>
<signal name="BrightnessChanged"/>
</interface></node>"#;
type Done = Box<dyn FnOnce(Result<f64, String>)>;
/// A no-device reset is idempotent success, not a hardware/policy receipt.
fn unavailable_policy_reset(
    policy: Policy,
    reset: bool,
    fixture: bool,
) -> Result<Option<Policy>, String> {
    if !reset || !policy.auto.is_finite() {
        return Err("no associated backlight for policy update".into());
    }
    Ok(fixture.then_some(policy))
}
/// Original grant values must still agree immediately before each helper.
/// The read closure performs the original device/range/owner checks; authority
/// and original request age are checked again after that observation.
fn original_pre_helper_readback(
    binding: &BrightnessBinding,
    reading: &BrightnessReading,
    started: std::time::Instant,
    mut live: impl FnMut() -> bool,
    read: impl FnOnce() -> Result<u32, String>,
    mut now: impl FnMut() -> std::time::Instant,
) -> Result<(), String> {
    if reading.binding != *binding || !journal::fresh(started, now()) || !live() {
        return Err("brightness original grant binding expired or revoked".into());
    }
    let actual = read()?;
    if actual != reading.applied
        || actual > binding.maximum
        || !journal::fresh(started, now())
        || !live()
    {
        return Err("brightness original grant readback changed or expired".into());
    }
    Ok(())
}

struct Completion {
    receipts: Vec<(Device, u32, u32, Option<u32>)>,
    observations: Vec<BrightnessObservation>,
    errors: Vec<String>,
    level: f64,
    user: bool,
    policy: Option<Policy>,
    permit: Option<Permit>,
    done: Done,
}
pub struct Controller {
    model: RefCell<Model>,
    journal: Rc<journal::Client>,
    outputs: Rc<dyn Fn() -> Vec<roost_shell_control::NativeOutputInfo>>,
    root: PathBuf,
    fixture_manifest: Option<PathBuf>,
    session: RefCell<Option<gio::DBusConnection>>,
    system: RefCell<Option<gio::DBusConnection>>,
    busy: Cell<bool>,
    available: Cell<bool>,
    idle_requested: Cell<Option<f64>>,
    idle_blocked_revision: Cell<Option<u64>>,
    settings: RefCell<Option<gio::Settings>>,
}
impl Controller {
    fn set_available(&self, available: bool) {
        if self.available.replace(available) == available {
            return;
        }
        if let Some(conn) = self.session.borrow().as_ref() {
            let values =
                std::collections::HashMap::from([("HasBrightnessControl", available.to_variant())]);
            let _ = conn.emit_signal(
                None,
                PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                Some(&(NAME, values, Vec::<String>::new()).to_variant()),
            );
        }
    }
    fn output_inventory(&self) -> Result<Vec<roost_shell_control::NativeOutputInfo>, String> {
        if let Some(path) = &self.fixture_manifest {
            return brightness::controlled_outputs(path);
        }
        Ok((self.outputs)())
    }
    pub fn journal_reply(&self, reply: roost_shell_host::control::BrightnessReply) {
        self.journal.receive(reply);
    }
    fn binding(device: &Device, generation: u64) -> Result<BrightnessBinding, String> {
        Ok(BrightnessBinding {
            output: device.owner.clone().ok_or("original output missing")?,
            ownership_generation: generation,
            backlight: device.name.clone(),
            device: device.dev,
            inode: device.inode,
            minimum: device.minimum(),
            maximum: device.max,
        })
    }
    fn invocation_source(
        &self,
        sender: Option<&str>,
        update: BrightnessPolicyUpdate,
    ) -> BrightnessSource {
        if let (Some(sender), Some(state)) = (sender, self.journal.snapshot()) {
            if state
                .provider
                .as_ref()
                .is_some_and(|v| v.unique == sender && v.epoch != 0)
            {
                return BrightnessSource::Power {
                    sender: sender.to_owned(),
                    update,
                };
            }
        }
        BrightnessSource::External { update }
    }
    fn refresh(self: &Rc<Self>) {
        if self.fixture_manifest.is_none()
            && !self
                .journal
                .snapshot()
                .is_some_and(|v| v.authority.is_some() && v.native_generation.is_some())
        {
            self.set_available(false);
            return;
        }
        if self.busy.get() {
            return;
        }
        let devices = self
            .output_inventory()
            .and_then(|outputs| brightness::inventory(&self.root, &outputs));
        if self.fixture_manifest.is_none() {
            let Some(state) = self.journal.snapshot() else {
                self.set_available(false);
                return;
            };
            let (Some(_authority), Some(generation)) = (state.authority, state.native_generation)
            else {
                self.set_available(false);
                return;
            };
            let Ok(devices) = devices else {
                self.set_available(false);
                return;
            };
            if self
                .model
                .borrow_mut()
                .import_journal(&state, devices.clone())
                .is_err()
            {
                self.set_available(false);
                if !self.journal.pending() {
                    let readings = devices
                        .iter()
                        .map(|device| {
                            Ok(BrightnessReading {
                                binding: Self::binding(device, generation)?,
                                user: device.current.max(device.minimum()),
                                user_known: false,
                                measured_candidate: None,
                                measured_restoration: None,
                                applied: device.current,
                                ratio: 1.0,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>();
                    if let Ok(readings) = readings {
                        let weak = Rc::downgrade(self);
                        self.journal.request(
                            JournalCommand::Initialize {
                                readings,
                                idle: self.model.borrow().policy.idle,
                            },
                            Box::new(move |result| {
                                if let Some(service) = weak.upgrade() {
                                    if result.is_ok() {
                                        service.refresh();
                                        service.drain_idle();
                                    } else {
                                        service.set_available(false);
                                    }
                                }
                            }),
                        );
                    }
                }
                return;
            }
            self.set_available(self.system.borrow().is_some() && self.model.borrow().available());
            return; // Import never replays DIM/auto or restores hardware.
        }
        let valid = devices.is_ok();
        let changed = match devices {
            Ok(devices) => self.model.borrow_mut().refresh(devices),
            Err(e) => {
                eprintln!("roost-shell-gtk: brightness inventory: {e}");
                // Preserve the user baseline across transient read failures.
                false
            }
        };
        let available = valid && self.system.borrow().is_some() && self.model.borrow().available();
        self.set_available(available);
        // Newly associated devices must obey an already-active policy too.
        let held = self.model.borrow().policy;
        if available && changed && (held.dimming || held.auto >= 0.0) {
            let targets = self.model.borrow().targets();
            if let Ok(targets) = targets {
                let pending: Vec<_> = targets
                    .into_iter()
                    .filter(|(d, n)| d.current != *n)
                    .map(|(d, n)| (d, n, None))
                    .collect();
                if !pending.is_empty() {
                    self.apply(
                        pending,
                        false,
                        None,
                        BrightnessSource::Automatic,
                        Box::new(|result| {
                            if let Err(e) = result {
                                eprintln!("roost-shell-gtk: held brightness policy: {e}");
                            }
                        }),
                    );
                }
            }
        }
    }
    pub fn has_output(self: &Rc<Self>, output: Option<&str>) -> bool {
        self.refresh();
        self.available.get() && self.model.borrow().has_output(output)
    }
    pub fn level(&self, output: Option<&str>) -> Option<f64> {
        self.model.borrow().user_level(output)
    }
    pub fn user(self: &Rc<Self>, output: Option<&str>, level: f64, done: Done) {
        self.refresh();
        if !self.available.get() {
            done(Err("no associated backlight".into()));
            return;
        }
        let targets = self.model.borrow().user_targets(output, level);
        match targets {
            Ok(targets) => self.apply(targets, true, None, BrightnessSource::User, done),
            Err(e) => done(Err(e)),
        }
    }
    pub fn step(
        self: &Rc<Self>,
        output: Option<&str>,
        step: crate::services::BrightnessStep,
        done: Done,
    ) {
        self.refresh();
        let Some(level) = self.level(output) else {
            done(Err("no associated backlight".into()));
            return;
        };
        let Some(increment) = self.model.borrow().step_size(output) else {
            done(Err("no brightness range".into()));
            return;
        };
        let next = match step {
            crate::services::BrightnessStep::Up => (level + increment).min(1.0),
            crate::services::BrightnessStep::Down => (level - increment).max(0.0),
            crate::services::BrightnessStep::Cycle => {
                if (1.0 - level).abs() < 0.001 {
                    0.0
                } else {
                    (level + increment).min(1.0)
                }
            }
        };
        self.user(output, next, done);
    }
    /// The controller owns the genuine subscription; its callback holds only a
    /// weak controller reference, so the listener does not form an ownership cycle.
    pub fn settings(self: &Rc<Self>, settings: gio::Settings) {
        let weak = Rc::downgrade(self);
        settings.connect_changed(Some("idle-brightness"), move |settings, _| {
            if let Some(service) = weak.upgrade() {
                service.idle(f64::from(settings.int("idle-brightness")) / 100.0);
            }
        });
        *self.settings.borrow_mut() = Some(settings);
    }
    /// Live installed idle policy. Coalesce changes during an owned hardware
    /// transaction, then apply the latest value after that actual completion.
    pub fn idle(self: &Rc<Self>, level: f64) {
        if !level.is_finite() {
            eprintln!("roost-shell-gtk: non-finite idle brightness");
            return;
        }
        self.idle_requested.set(Some(level.clamp(0.0, 1.0)));
        self.idle_blocked_revision.set(None);
        self.drain_idle();
    }
    fn drain_idle(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        self.refresh();
        if self.busy.get() || !self.available.get() {
            return;
        }
        let revision = self.journal.snapshot().map(|v| v.revision);
        if self.fixture_manifest.is_none()
            && self.idle_blocked_revision.get() == revision
            && revision.is_some()
        {
            return;
        }
        let Some(idle) = self.idle_requested.take() else {
            return;
        };
        let mut policy = self.model.borrow().policy;
        policy.idle = idle;
        let weak = Rc::downgrade(self);
        self.policy(
            policy,
            true,
            BrightnessSource::Idle(idle),
            Box::new(move |result| {
                if let Some(service) = weak.upgrade() {
                    if result.is_err() {
                        // Retain the last installed user setting, but do not replay
                        // a failed/unknown transaction on every idle callback.
                        if service.idle_requested.get().is_none() {
                            service.idle_requested.set(Some(idle));
                        }
                        service.idle_blocked_revision.set(revision);
                    } else {
                        service.idle_blocked_revision.set(None);
                    }
                }
            }),
        );
    }
    fn completed(self: &Rc<Self>) {
        self.busy.set(false);
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(service) = weak.upgrade() {
                service.drain_idle();
            }
        });
    }
    fn policy(self: &Rc<Self>, policy: Policy, reset: bool, source: BrightnessSource, done: Done) {
        self.refresh();
        if !policy.auto.is_finite() {
            done(Err("non-finite automatic brightness target".into()));
            return;
        }
        if !self.available.get() {
            let result = unavailable_policy_reset(policy, reset, self.fixture_manifest.is_some());
            match result {
                Ok(Some(policy)) => {
                    self.model.borrow_mut().policy = policy;
                    done(Ok(0.0));
                }
                Ok(None) => done(Ok(0.0)), // no native state, grant or helper fabricated
                Err(error) => done(Err(error)),
            }
            return;
        }
        let old = self.model.borrow().policy;
        self.model.borrow_mut().policy = policy;
        let targets = self.model.borrow().targets();
        self.model.borrow_mut().policy = old;
        match targets {
            Ok(targets) => self.apply(
                targets.into_iter().map(|(d, n)| (d, n, None)).collect(),
                false,
                Some(policy),
                source,
                done,
            ),
            Err(e) => done(Err(e)),
        }
    }
    /// One bounded batch at a time. A failed request never reports its requested
    /// percentage as observed; partial hardware writes are returned as failures.
    fn apply(
        self: &Rc<Self>,
        targets: Vec<(Device, u32, Option<u32>)>,
        user: bool,
        policy: Option<Policy>,
        source: BrightnessSource,
        done: Done,
    ) {
        if targets.is_empty() {
            done(Err("no associated backlight".into()));
            return;
        }
        if self.busy.replace(true) {
            done(Err("brightness update already pending".into()));
            return;
        }
        if self.fixture_manifest.is_some() {
            self.apply_granted(targets, user, policy, None, None, done);
            return;
        }
        let Some(state) = self.journal.snapshot() else {
            self.completed();
            done(Err("brightness journal unavailable".into()));
            return;
        };
        let (Some(authority), Some(generation)) = (state.authority, state.native_generation) else {
            self.completed();
            done(Err("brightness native authority unavailable".into()));
            return;
        };
        let wanted = targets
            .iter()
            .map(|(device, effective, desired)| {
                Ok(BrightnessTarget {
                    binding: Self::binding(device, generation)?,
                    effective: *effective,
                    user: *desired,
                    ratio: None,
                })
            })
            .collect::<Result<Vec<_>, String>>();
        let Ok(wanted) = wanted else {
            self.completed();
            done(Err("brightness original binding unavailable".into()));
            return;
        };
        let service = self.clone();
        self.journal.request(
            JournalCommand::Begin {
                source,
                targets: wanted,
            },
            Box::new(move |result| match result {
                Ok(receipt) => {
                    let Some(grant) = receipt.reply.grant else {
                        service.completed();
                        done(Err("brightness original grant missing".into()));
                        return;
                    };
                    let permit = Permit {
                        authority,
                        generation,
                        grant,
                        admitted: receipt.started,
                    };
                    if !service.journal.may_submit(permit) {
                        service.completed();
                        done(Err("brightness original grant revoked".into()));
                        return;
                    }
                    let accepted = receipt.reply.state.map(|state| state.readings);
                    service.apply_granted(targets, user, policy, Some(permit), accepted, done);
                }
                Err(error) => {
                    service.completed();
                    done(Err(error));
                }
            }),
        );
    }
    fn finish_commit(self: &Rc<Self>, mut completion: Completion, ack: Result<(), String>) {
        if let Err(error) = ack {
            completion.errors.push(error);
        }
        if completion.errors.is_empty() {
            if let Err(error) = self
                .model
                .borrow_mut()
                .acknowledge_batch(&completion.receipts)
            {
                completion.errors.push(error);
            }
        }
        if completion.errors.is_empty() {
            if let Some(policy) = completion.policy {
                self.model.borrow_mut().policy = policy;
            }
            if completion.user {
                self.model.borrow_mut().sync_factors();
                if let Some(conn) = self.session.borrow().as_ref() {
                    let _ = conn.emit_signal(
                        None,
                        PATH,
                        NAME,
                        "BrightnessChanged",
                        Some(&().to_variant()),
                    );
                }
            }
        }
        self.completed();
        if completion.errors.is_empty() {
            (completion.done)(Ok(completion.level));
        } else {
            (completion.done)(Err(completion.errors.join("; ")));
        }
    }
    fn finish_batch(self: &Rc<Self>, completion: Completion) {
        let Some(permit) = completion.permit else {
            // Existing explicitly controlled sysfs/helper fixture only; never
            // represented as compositor-owned native journal qualification.
            self.finish_commit(completion, Ok(()));
            return;
        };
        if !self.journal.same_original(permit) {
            self.finish_commit(
                completion,
                Err("brightness completion authority revoked".into()),
            );
            return;
        }
        let observations = completion.observations.clone();
        let service = self.clone();
        self.journal.request(
            JournalCommand::Complete {
                grant: permit.grant,
                observations,
            },
            Box::new(move |reply| {
                service.finish_commit(completion, reply.map(|_| ()));
            }),
        );
    }
    fn apply_granted(
        self: &Rc<Self>,
        targets: Vec<(Device, u32, Option<u32>)>,
        user: bool,
        policy: Option<Policy>,
        permit: Option<Permit>,
        accepted: Option<Vec<BrightnessReading>>,
        done: Done,
    ) {
        let Some(conn) = self.system.borrow().clone() else {
            self.finish_batch(Completion {
                receipts: Vec::new(),
                observations: Vec::new(),
                errors: vec!["system bus unavailable".into()],
                level: 0.0,
                user,
                policy,
                permit,
                done,
            });
            return;
        };
        struct Batch {
            left: usize,
            receipts: Vec<(Device, u32, u32, Option<u32>)>,
            observations: Vec<BrightnessObservation>,
            errors: Vec<String>,
            level: f64,
            done: Option<Done>,
        }
        impl Batch {
            fn take_completion(
                &mut self,
                user: bool,
                policy: Option<Policy>,
                permit: Option<Permit>,
            ) -> Completion {
                Completion {
                    receipts: std::mem::take(&mut self.receipts),
                    observations: std::mem::take(&mut self.observations),
                    errors: std::mem::take(&mut self.errors),
                    level: self.level,
                    user,
                    policy,
                    permit,
                    done: self.done.take().unwrap(),
                }
            }
        }
        let batch = Rc::new(RefCell::new(Batch {
            left: targets.len(),
            receipts: Vec::with_capacity(targets.len()),
            observations: Vec::with_capacity(targets.len()),
            errors: Vec::new(),
            level: 0.0,
            done: Some(done),
        }));
        for (device, target, desired_user) in targets {
            let service = self.clone();
            let batch = batch.clone();
            let ready = if let Some(permit) = permit {
                Self::binding(&device, permit.generation).and_then(|binding| {
                    let original = accepted
                        .as_ref()
                        .and_then(|readings| {
                            readings.iter().find(|reading| reading.binding == binding)
                        })
                        .ok_or("brightness original grant reading missing")?;
                    original_pre_helper_readback(
                        &binding,
                        original,
                        permit.admitted,
                        || self.journal.same_original(permit),
                        || brightness::readback(&device),
                        std::time::Instant::now,
                    )
                })
            } else {
                brightness::readback(&device).map(|_| ()) // controlled fixture path only
            };
            if let Err(error) = ready {
                let finished = {
                    let mut batch = batch.borrow_mut();
                    batch.errors.push(error);
                    batch.left -= 1;
                    (batch.left == 0).then(|| batch.take_completion(user, policy, permit))
                };
                if let Some(finished) = finished {
                    service.finish_batch(finished);
                }
                continue;
            }
            conn.call(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1/session/auto",
                "org.freedesktop.login1.Session",
                "SetBrightness",
                Some(&("backlight", device.name.as_str(), target).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2000,
                gio::Cancellable::NONE,
                move |reply| {
                    let actual = (|| {
                        if !service
                            .output_inventory()?
                            .iter()
                            .any(|o| Some(o) == device.owner.as_ref())
                        {
                            return Err("output disappeared during brightness update".to_owned());
                        }
                        let actual = brightness::readback(&device)?;
                        service
                            .model
                            .borrow_mut()
                            .observe_failure(&device, actual)?;
                        Ok(actual)
                    })();
                    let finished = {
                        let mut batch = batch.borrow_mut();
                        match actual {
                            Err(error) => batch.errors.push(error),
                            Ok(actual) => {
                                if let Some(permit) = permit {
                                    match Self::binding(&device, permit.generation) {
                                        Ok(binding) => {
                                            batch.observations.push(BrightnessObservation {
                                                binding,
                                                actual,
                                                helper_succeeded: reply.is_ok(),
                                            })
                                        }
                                        Err(error) => batch.errors.push(error),
                                    }
                                }
                                match reply {
                                    Err(error) => batch.errors.push(error.to_string()),
                                    Ok(_) if actual == target => {
                                        batch.level = batch.level.max(device.relative(actual));
                                        batch.receipts.push((
                                            device.clone(),
                                            target,
                                            actual,
                                            desired_user,
                                        ));
                                    }
                                    Ok(_) => {
                                        batch.errors.push("backlight readback mismatch".into())
                                    }
                                }
                            }
                        }
                        batch.left -= 1;
                        (batch.left == 0).then(|| batch.take_completion(user, policy, permit))
                    };
                    if let Some(finished) = finished {
                        service.finish_batch(finished);
                    }
                },
            );
        }
    }
}

pub fn start(
    bridge: journal::Bridge,
    outputs: Rc<dyn Fn() -> Vec<roost_shell_control::NativeOutputInfo>>,
    idle: f64,
) -> Rc<Controller> {
    let mut model = Model::default();
    model.policy.idle = idle.clamp(0.0, 1.0);
    let service = Rc::new(Controller {
        model: RefCell::new(model),
        journal: journal::Client::new(bridge),
        outputs,
        fixture_manifest: if std::env::var_os("ROOST_BACKLIGHT_ROOT").is_some() {
            std::env::var_os("ROOST_BACKLIGHT_TEST_CONNECTORS").map(PathBuf::from)
        } else {
            None
        },
        root: std::env::var_os("ROOST_BACKLIGHT_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/sys/class/backlight".into()),
        session: RefCell::new(None),
        system: RefCell::new(None),
        busy: Cell::new(false),
        available: Cell::new(false),
        idle_requested: Cell::new(None),
        idle_blocked_revision: Cell::new(None),
        settings: RefCell::new(None),
    });
    let weak = Rc::downgrade(&service);
    gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |conn| {
        if let Some(service) = weak.upgrade() {
            match conn {
                Ok(conn) => *service.system.borrow_mut() = Some(conn),
                Err(e) => eprintln!("roost-shell-gtk: brightness system bus: {e}"),
            }
            service.refresh();
        }
    });
    let info = gio::DBusNodeInfo::for_xml(XML)
        .expect("brightness interface XML")
        .lookup_interface(NAME)
        .expect("brightness interface");
    let svc = service.clone();
    let _owner = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            *svc.session.borrow_mut() = Some(conn.clone());
            let methods = svc.clone();
            let properties = svc.clone();
            let result = conn
                .register_object(PATH, &info)
                .method_call(move |_, sender, _, _, method, params, invocation| {
                    // Observe external user changes before deriving the next policy.
                    methods.refresh();
                    let mut policy = methods.model.borrow().policy;
                    let reset = match method {
                        "SetDimming" => {
                            let Some((value,)) = params.get::<(bool,)>() else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    "expected boolean",
                                );
                                return;
                            };
                            policy.dimming = value;
                            !value
                        }
                        "SetAutoBrightnessTarget" => {
                            let Some((value,)) = params.get::<(f64,)>() else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    "expected double",
                                );
                                return;
                            };
                            if !value.is_finite() {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    "non-finite target",
                                );
                                return;
                            }
                            policy.auto = value;
                            value < 0.0
                        }
                        _ => {
                            invocation.return_dbus_error(
                                "org.freedesktop.DBus.Error.UnknownMethod",
                                method,
                            );
                            return;
                        }
                    };
                    let update = match method {
                        "SetDimming" => BrightnessPolicyUpdate::Dimming(policy.dimming),
                        _ => BrightnessPolicyUpdate::Automatic(policy.auto),
                    };
                    let source = methods.invocation_source(sender, update);
                    methods.policy(
                        policy,
                        reset,
                        source,
                        Box::new(move |result| match result {
                            Ok(_) => invocation.return_value(Some(&().to_variant())),
                            Err(e) => invocation
                                .return_dbus_error("org.gnome.Shell.Brightness.Error.Failed", &e),
                        }),
                    );
                })
                .property(move |_, _, _, _, name| match name {
                    "HasBrightnessControl" => {
                        properties.refresh();
                        properties.available.get().to_variant()
                    }
                    _ => ().to_variant(),
                })
                .build();
            if let Err(e) = result {
                eprintln!("roost-shell-gtk: brightness DBus registration: {e}");
            }
        },
        |_, _| {},
        |_, _| {},
    );
    let weak = Rc::downgrade(&service);
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let Some(service) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        service.refresh();
        service.drain_idle();
        glib::ControlFlow::Continue
    });
    service
}

#[cfg(test)]
mod original_admission_tests {
    use super::*;
    use std::time::Instant;
    fn reading() -> BrightnessReading {
        BrightnessReading {
            binding: BrightnessBinding {
                output: roost_shell_control::NativeOutputInfo {
                    name: "eDP-1".into(),
                    drm_device: 226,
                    connector_id: 1,
                    connector_sysfs: "/sys/devices/card0-eDP-1".into(),
                    connector_device: 1,
                    connector_inode: 2,
                },
                ownership_generation: 1,
                backlight: "panel".into(),
                device: 1,
                inode: 4,
                minimum: 1,
                maximum: 1000,
            },
            user: 700,
            user_known: false,
            measured_candidate: None,
            measured_restoration: None,
            applied: 210,
            ratio: 1.0,
        }
    }
    #[test]
    fn no_device_native_reset_ack_has_no_policy_or_hardware_authority_receipt() {
        let policy = Policy::default();
        assert!(unavailable_policy_reset(policy, true, false)
            .unwrap()
            .is_none());
        let fixture = unavailable_policy_reset(policy, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(fixture.auto, policy.auto);
        assert_eq!(fixture.dimming, policy.dimming);
        assert!(unavailable_policy_reset(policy, false, false).is_err());
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(unavailable_policy_reset(
                Policy {
                    auto: value,
                    ..policy
                },
                true,
                false
            )
            .is_err());
        }
    }
    #[test]
    fn changed_original_level_or_binding_stops_helper_admission() {
        let original = reading();
        let now = Instant::now();
        assert!(original_pre_helper_readback(
            &original.binding,
            &original,
            now,
            || true,
            || Ok(210),
            || now
        )
        .is_ok());
        assert!(original_pre_helper_readback(
            &original.binding,
            &original,
            now,
            || true,
            || Ok(100),
            || now
        )
        .is_err());
        let mut rebound = original.binding.clone();
        rebound.inode += 1;
        let read_called = Cell::new(false);
        assert!(original_pre_helper_readback(
            &rebound,
            &original,
            now,
            || true,
            || {
                read_called.set(true);
                Ok(210)
            },
            || now
        )
        .is_err());
        assert!(!read_called.get());
    }
    #[test]
    fn original_budget_and_authority_are_checked_after_readback_without_renewal() {
        let original = reading();
        let started = Instant::now();
        let time = Cell::new(started + Duration::from_millis(1900));
        assert!(original_pre_helper_readback(
            &original.binding,
            &original,
            started,
            || true,
            || {
                time.set(started + Duration::from_millis(2001));
                Ok(210)
            },
            || time.get()
        )
        .is_err());
        let live = Cell::new(true);
        assert!(original_pre_helper_readback(
            &original.binding,
            &original,
            started,
            || live.get(),
            || {
                live.set(false);
                Ok(210)
            },
            || started
        )
        .is_err());
    }
}
