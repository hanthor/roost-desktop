//! GNOME's calendar events (calendar.js `DBusEventSource`): the session
//! bus's org.gnome.Shell.CalendarServer, activated on demand, asked for
//! the calendar grid's range and followed through its signals. The date
//! menu marks days with events and lists the selected day's.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gio::prelude::*;

use crate::logic::{self, CalEvent};

const NAME: &str = "org.gnome.Shell.CalendarServer";
const PATH: &str = "/org/gnome/Shell/CalendarServer";
const IFACE: &str = "org.gnome.Shell.CalendarServer";

/// The calendar server's events, as GNOME's shell caches them.
#[derive(Default)]
pub struct EventSource {
    conn: RefCell<Option<gio::DBusConnection>>,
    owned: Cell<bool>,
    events: RefCell<BTreeMap<String, CalEvent>>,
    range: Cell<Option<(i64, i64)>>,
    has_calendars: Cell<bool>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
}

impl EventSource {
    /// Connect to the session bus and start following the server.
    pub fn new() -> Rc<Self> {
        let source = Rc::new(Self::default());
        let weak = Rc::downgrade(&source);
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| {
                let (Ok(conn), Some(source)) = (conn, weak.upgrade()) else {
                    return;
                };
                source.attach(&conn);
            },
        );
        source
    }

    /// Call `f` whenever the events or HasCalendars change.
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        *self.changed.borrow_mut() = Some(Rc::new(f));
    }

    /// Whether the server has calendars (GNOME hides the events card
    /// without them).
    pub fn has_calendars(&self) -> bool {
        self.has_calendars.get()
    }

    /// The events overlapping `[begin, end)`, GNOME's order.
    pub fn events_between(&self, begin: i64, end: i64) -> Vec<CalEvent> {
        logic::events_between(self.events.borrow().values(), begin, end)
    }

    /// Ask for the calendar grid's range (reloading when it changes).
    pub fn request_range(&self, begin: i64, end: i64) {
        if self.range.get() == Some((begin, end)) {
            return;
        }
        self.range.set(Some((begin, end)));
        self.load(true);
    }

    fn emit(&self) {
        let f = self.changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    fn attach(self: &Rc<Self>, conn: &gio::DBusConnection) {
        *self.conn.borrow_mut() = Some(conn.clone());
        let weak = Rc::downgrade(self);
        let sub = conn.subscribe_to_signal(
            Some(NAME),
            None,
            None,
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if let Some(source) = weak.upgrade() {
                    source.on_signal(signal.interface_name, signal.signal_name, signal.parameters);
                }
            },
        );
        // The subscription lives as long as the shell.
        std::mem::forget(sub);
        // Start the server, as GNOME's proxy activates it, and follow
        // its owner (a NameOwnerChanged watch: gio's name watcher hands
        // its callbacks a NULL connection when the bus goes away).
        conn.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "StartServiceByName",
            Some(&(NAME, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |_| {},
        );
        let weak = Rc::downgrade(self);
        crate::services::watch_name(conn, NAME, move |owned| {
            let Some(source) = weak.upgrade() else {
                return;
            };
            source.owned.set(owned);
            source.events.borrow_mut().clear();
            if owned {
                source.refresh_has_calendars();
                source.load(true);
            } else {
                source.has_calendars.set(false);
                source.emit();
            }
        });
    }

    fn on_signal(self: &Rc<Self>, iface: &str, name: &str, params: &glib::Variant) {
        if iface == "org.freedesktop.DBus.Properties" {
            if name == "PropertiesChanged" {
                self.refresh_has_calendars();
            }
            return;
        }
        if iface != IFACE {
            return;
        }
        let changed = match name {
            "EventsAddedOrUpdated" => {
                let added: Vec<CalEvent> = params
                    .try_child_value(0)
                    .map(|list| {
                        list.iter()
                            .filter_map(|item| {
                                let (id, summary, start, end, _extras) =
                                    item.get::<(String, String, i64, i64, glib::VariantDict)>()?;
                                Some(CalEvent {
                                    id,
                                    summary,
                                    start,
                                    end,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let any = !added.is_empty();
                logic::events_added(&mut self.events.borrow_mut(), added);
                any
            }
            "EventsRemoved" => {
                let ids: Vec<String> = params
                    .try_child_value(0)
                    .and_then(|v| v.get::<Vec<String>>())
                    .unwrap_or_default();
                let mut events = self.events.borrow_mut();
                {
                    // Every id is applied: no short circuit.
                    let mut any = false;
                    for id in &ids {
                        any |= logic::events_remove_matching(&mut events, id);
                    }
                    any
                }
            }
            "ClientDisappeared" => {
                let uid = params
                    .try_child_value(0)
                    .and_then(|v| v.get::<String>())
                    .unwrap_or_default();
                logic::events_remove_matching(&mut self.events.borrow_mut(), &format!("{uid}\n"))
            }
            _ => false,
        };
        if changed {
            self.emit();
        }
    }

    /// Read HasCalendars (GNOME re-reads it on PropertiesChanged).
    fn refresh_has_calendars(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        conn.call(
            Some(NAME),
            PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(IFACE, "HasCalendars").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| {
                let Some(source) = weak.upgrade() else {
                    return;
                };
                let has = reply
                    .ok()
                    .and_then(|v| v.child_value(0).as_variant())
                    .and_then(|v| v.get::<bool>())
                    .unwrap_or(false);
                if source.has_calendars.replace(has) != has {
                    source.emit();
                }
            },
        );
    }

    /// GNOME's `_loadEvents`: SetTimeRange over the requested range.
    fn load(&self, force: bool) {
        let (Some(conn), Some((begin, end))) = (self.conn.borrow().clone(), self.range.get())
        else {
            return;
        };
        if !self.owned.get() {
            return;
        }
        if force {
            self.events.borrow_mut().clear();
            self.emit();
        }
        conn.call(
            Some(NAME),
            PATH,
            IFACE,
            "SetTimeRange",
            Some(&(begin, end, force).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
}
