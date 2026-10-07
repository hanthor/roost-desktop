//! The GNOME 51 brightness contract. Positive capability requires an actual
//! associated backlight; synthetic test roots are never native qualification.
use crate::brightness::{self, Device, Model, Policy};
use gio::prelude::*;
use glib::variant::ToVariant;
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
pub struct Controller {
    model: RefCell<Model>,
    outputs: Rc<dyn Fn() -> Vec<roost_shell_control::NativeOutputInfo>>,
    root: PathBuf,
    fixture_manifest: Option<PathBuf>,
    session: RefCell<Option<gio::DBusConnection>>,
    system: RefCell<Option<gio::DBusConnection>>,
    busy: Cell<bool>,
    available: Cell<bool>,
    idle_requested: Cell<Option<f64>>,
    settings: RefCell<Option<gio::Settings>>,
}
impl Controller {
    fn output_inventory(&self) -> Result<Vec<roost_shell_control::NativeOutputInfo>, String> {
        if let Some(path) = &self.fixture_manifest {
            return brightness::controlled_outputs(path);
        }
        Ok((self.outputs)())
    }
    fn refresh(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let devices = self
            .output_inventory()
            .and_then(|outputs| brightness::inventory(&self.root, &outputs));
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
        if self.available.replace(available) != available {
            if let Some(conn) = self.session.borrow().as_ref() {
                let values = std::collections::HashMap::from([(
                    "HasBrightnessControl",
                    available.to_variant(),
                )]);
                let _ = conn.emit_signal(
                    None,
                    PATH,
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    Some(&(NAME, values, Vec::<String>::new()).to_variant()),
                );
            }
        }
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
            Ok(targets) => self.apply(targets, true, None, done),
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
        self.drain_idle();
    }
    fn drain_idle(self: &Rc<Self>) {
        if self.busy.get() {
            return;
        }
        let Some(idle) = self.idle_requested.take() else {
            return;
        };
        self.refresh();
        if self.busy.get() {
            self.idle_requested.set(Some(idle));
            return;
        }
        let mut policy = self.model.borrow().policy;
        policy.idle = idle;
        self.policy(
            policy,
            true,
            Box::new(|result| {
                if let Err(error) = result {
                    eprintln!("roost-shell-gtk: live idle brightness: {error}");
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
    fn policy(self: &Rc<Self>, policy: Policy, reset: bool, done: Done) {
        self.refresh();
        if !policy.auto.is_finite() {
            done(Err("non-finite automatic brightness target".into()));
            return;
        }
        if !self.available.get() {
            if reset {
                self.model.borrow_mut().policy = policy;
                done(Ok(0.0));
            } else {
                done(Err("no associated backlight".into()));
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
        let Some(conn) = self.system.borrow().clone() else {
            self.busy.set(false);
            done(Err("system bus unavailable".into()));
            return;
        };
        struct Batch {
            left: usize,
            error: Vec<String>,
            level: f64,
            done: Option<Done>,
        }
        let batch = Rc::new(RefCell::new(Batch {
            left: targets.len(),
            error: Vec::new(),
            level: 0.0,
            done: Some(done),
        }));
        for (device, target, desired_user) in targets {
            let service = self.clone();
            let batch = batch.clone();
            // Verify original identity/range immediately before admission too.
            if let Err(e) = brightness::readback(&device) {
                let mut b = batch.borrow_mut();
                b.error.push(e);
                b.left -= 1;
                if b.left == 0 {
                    service.completed();
                    b.done.take().unwrap()(Err(b.error.join("; ")));
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
                    let result = (|| {
                        if !service
                            .output_inventory()?
                            .iter()
                            .any(|o| Some(o) == device.owner.as_ref())
                        {
                            return Err("output disappeared during brightness update".into());
                        }
                        let actual = brightness::readback(&device)?;
                        match reply {
                            Err(error) => {
                                service
                                    .model
                                    .borrow_mut()
                                    .observe_failure(&device, actual)?;
                                Err(error.to_string())
                            }
                            Ok(_) => service.model.borrow_mut().acknowledge(
                                &device,
                                target,
                                actual,
                                desired_user,
                            ),
                        }
                    })();
                    let mut b = batch.borrow_mut();
                    match result {
                        Ok(level) => b.level = b.level.max(level),
                        Err(e) => b.error.push(e),
                    }
                    b.left -= 1;
                    if b.left == 0 {
                        service.completed();
                        let result = if b.error.is_empty() {
                            if let Some(policy) = policy {
                                service.model.borrow_mut().policy = policy;
                            }
                            if user {
                                service.model.borrow_mut().sync_factors();
                                if let Some(conn) = service.session.borrow().as_ref() {
                                    let _ = conn.emit_signal(
                                        None,
                                        PATH,
                                        NAME,
                                        "BrightnessChanged",
                                        Some(&().to_variant()),
                                    );
                                }
                            }
                            Ok(b.level)
                        } else {
                            Err(b.error.join("; "))
                        };
                        b.done.take().unwrap()(result);
                    }
                },
            );
        }
    }
}

pub fn start(
    outputs: Rc<dyn Fn() -> Vec<roost_shell_control::NativeOutputInfo>>,
    idle: f64,
) -> Rc<Controller> {
    let mut model = Model::default();
    model.policy.idle = idle.clamp(0.0, 1.0);
    let service = Rc::new(Controller {
        model: RefCell::new(model),
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
                .method_call(move |_, _, _, _, method, params, invocation| {
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
                    methods.policy(
                        policy,
                        reset,
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
        glib::ControlFlow::Continue
    });
    service
}
