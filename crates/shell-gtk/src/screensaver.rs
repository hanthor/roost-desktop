//! GNOME's screen-saver API, backed by the compositor's confirmed lock state.
//! A bus caller can request a lock, but cannot bypass password authentication.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::{gio, glib, prelude::*};

const NAME: &str = "org.gnome.ScreenSaver";
const PATH: &str = "/org/gnome/ScreenSaver";
const XML: &str = r#"<node><interface name="org.gnome.ScreenSaver">
  <method name="Lock"/>
  <method name="GetActive"><arg type="b" direction="out"/></method>
  <method name="SetActive"><arg type="b" direction="in"/></method>
  <method name="GetActiveTime"><arg type="u" direction="out"/></method>
  <signal name="ActiveChanged"><arg type="b"/></signal>
</interface></node>"#;

type Lock = Rc<dyn Fn() -> Result<(), &'static str>>;
type Active = Rc<dyn Fn() -> bool>;

pub struct Service {
    conn: RefCell<Option<gio::DBusConnection>>,
    since: RefCell<Option<Instant>>,
}

impl Service {
    /// Publish transitions after the control client has received lock state.
    pub fn sync(&self, active: bool) {
        let mut since = self.since.borrow_mut();
        if active == since.is_some() {
            return;
        }
        *since = active.then(Instant::now);
        if let Some(conn) = self.conn.borrow().as_ref() {
            let _ = conn.emit_signal(
                None,
                PATH,
                NAME,
                "ActiveChanged",
                Some(&(active,).to_variant()),
            );
        }
    }
}

pub fn start(lock: Lock, active: Active) -> Rc<Service> {
    let service = Rc::new(Service {
        conn: RefCell::new(None),
        since: RefCell::new(None),
    });
    let info = gio::DBusNodeInfo::for_xml(XML)
        .expect("screen-saver interface XML")
        .lookup_interface(NAME)
        .expect("screen-saver interface");
    let svc = service.clone();
    // GIO uses the same shared session connection as org.gnome.Shell, so
    // the object is reachable through both names, as GNOME components expect.
    let _owner = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            *svc.conn.borrow_mut() = Some(conn.clone());
            let (lock, active, svc) = (lock.clone(), active.clone(), svc.clone());
            let result = conn
                .register_object(PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    match method {
                        "GetActive" => invocation.return_value(Some(&(active(),).to_variant())),
                        "GetActiveTime" => {
                            let seconds = svc
                                .since
                                .borrow()
                                .map_or(0, |t| t.elapsed().as_secs().min(u32::MAX as u64) as u32);
                            invocation.return_value(Some(&(seconds,).to_variant()));
                        }
                        "Lock" | "SetActive" => {
                            let requested = method == "Lock"
                                || params.get::<(bool,)>().is_some_and(|(value,)| value);
                            // SetActive(false) never unlocks a locked session.
                            if !requested || active() {
                                invocation.return_value(None);
                                return;
                            }
                            if let Err(message) = lock() {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.Failed",
                                    message,
                                );
                                return;
                            }
                            let active = active.clone();
                            let started = Instant::now();
                            let mut reply = Some(invocation);
                            glib::timeout_add_local(Duration::from_millis(50), move || {
                                if active() {
                                    reply.take().unwrap().return_value(None);
                                    glib::ControlFlow::Break
                                } else if started.elapsed() >= Duration::from_secs(5) {
                                    reply.take().unwrap().return_dbus_error(
                                        "org.freedesktop.DBus.Error.Failed",
                                        "compositor did not confirm the lock",
                                    );
                                    glib::ControlFlow::Break
                                } else {
                                    glib::ControlFlow::Continue
                                }
                            });
                        }
                        _ => invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                    }
                })
                .build();
            if let Err(error) = result {
                eprintln!("tuna-shell-gtk: screen-saver interface: {error}");
            }
        },
        |_, _| {},
        |_, _| eprintln!("tuna-shell-gtk: screen-saver name owned elsewhere"),
    );
    service
}
