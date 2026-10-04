//! GNOME's wired toggle and its menu (network.js `NMWiredToggle`): the
//! ethernet devices NetworkManager lists, each with its saved profiles.
//! The toggle disconnects whatever is up, or brings up the most recently
//! used profile (a new automatic one when there is none); the menu picks
//! a profile, and Wired Settings opens GNOME Settings.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::logic::{self, WiredAction, WiredConnection, WiredDevice, WiredRow};
use crate::services::Tile;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
/// `NM_DEVICE_TYPE_ETHERNET`.
const ETHERNET: u32 = 1;
/// libnm's generic name for an ethernet device (`disambiguate_names`).
const DEVICE_NAME: &str = "Wired";

/// The wired toggle's menu and the state behind it.
pub struct WiredMenu {
    list: gtk::Box,
    tile: RefCell<Option<Tile>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    devices: RefCell<Vec<WiredDevice>>,
    connectivity: Cell<u32>,
    pending: Cell<bool>,
    /// Bumped on every reload, so a slower earlier one cannot win.
    generation: Cell<u64>,
}

impl WiredMenu {
    /// The menu's rows go into `list`.
    pub fn new(list: gtk::Box) -> Rc<Self> {
        Rc::new(Self {
            list,
            tile: RefCell::default(),
            conn: RefCell::default(),
            devices: RefCell::default(),
            connectivity: Cell::new(0),
            pending: Cell::new(false),
            generation: Cell::new(0),
        })
    }

    /// The toggle that follows the devices; a click runs GNOME's
    /// `NMToggle.activate`.
    pub fn set_tile(self: &Rc<Self>, tile: Tile) {
        let weak = Rc::downgrade(self);
        tile.on_user_toggle(move |_| {
            if let Some(menu) = weak.upgrade() {
                for (device, action) in logic::wired_click(&menu.devices.borrow()) {
                    menu.run(&device, &action);
                }
                // The tile shows NetworkManager's state, not the click.
                menu.sync_tile();
            }
        });
        *self.tile.borrow_mut() = Some(tile);
        self.sync_tile();
    }

    /// NetworkManager on `conn`: read it now and on every change.
    pub fn attach(self: &Rc<Self>, conn: &gio::DBusConnection) {
        if self.conn.replace(Some(conn.clone())).is_none() {
            for (iface, member) in [
                (PROPS, "PropertiesChanged"),
                (NM, "DeviceAdded"),
                (NM, "DeviceRemoved"),
            ] {
                let weak = Rc::downgrade(self);
                let sub = conn.subscribe_to_signal(
                    Some(NM),
                    Some(iface),
                    Some(member),
                    None,
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |_| {
                        if let Some(menu) = weak.upgrade() {
                            menu.schedule_reload();
                        }
                    },
                );
                // The subscriptions live as long as the shell.
                std::mem::forget(sub);
            }
        }
        self.schedule_reload();
    }

    /// NetworkManager went away: no devices.
    pub fn detach(self: &Rc<Self>) {
        self.generation.set(self.generation.get() + 1);
        self.devices.borrow_mut().clear();
        self.render();
    }

    fn schedule_reload(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(menu) = weak.upgrade() {
                menu.pending.set(false);
                menu.reload();
            }
        });
    }

    /// Read connectivity, the ethernet devices, their profiles and
    /// active connections, then rebuild.
    fn reload(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        let conn2 = conn.clone();
        get_all(&conn, NM_PATH, NM, move |nm| {
            let Some(nm) = nm else { return };
            let connectivity = nm
                .lookup_value("Connectivity", None)
                .and_then(|v| v.get::<u32>())
                .unwrap_or(0);
            let primary = object_path(&nm, "PrimaryConnection");
            let networking = nm
                .lookup_value("NetworkingEnabled", None)
                .and_then(|v| v.get::<bool>())
                .unwrap_or(true);
            let conn3 = conn2.clone();
            conn2.call(
                Some(NM),
                NM_PATH,
                NM,
                "GetDevices",
                None,
                glib::VariantTy::new("(ao)").ok(),
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
                move |reply| {
                    let paths: Vec<String> = if networking {
                        reply
                            .ok()
                            .and_then(|v| v.child_value(0).get::<Vec<glib::variant::ObjectPath>>())
                            .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    let found: Rc<RefCell<Vec<WiredDevice>>> = Rc::default();
                    let done = Countdown::new(paths.len() + 1, {
                        let (weak, found) = (weak.clone(), found.clone());
                        move || {
                            let Some(menu) = weak.upgrade() else { return };
                            if menu.generation.get() != generation {
                                return;
                            }
                            let mut devices = found.take();
                            devices.sort_by(|a, b| a.path.cmp(&b.path));
                            menu.connectivity.set(connectivity);
                            *menu.devices.borrow_mut() = devices;
                            menu.render();
                        }
                    });
                    for path in paths {
                        read_device(&conn3, path, primary.clone(), found.clone(), done.clone());
                    }
                    done.tick();
                },
            );
        });
    }

    fn render(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let devices = self.devices.borrow().clone();
        let connectivity = self.connectivity.get();
        for dev in devices.iter().filter(|d| d.shown()) {
            for row in dev.menu(DEVICE_NAME) {
                let button = self.row(dev, &row, connectivity);
                self.list.append(&button);
            }
        }
        self.sync_tile();
    }

    /// One menu row as GNOME draws it (`NMConnectionItem`): the device's
    /// icon, its name and a Connect/Disconnect subtitle; radio rows are
    /// the profile's name with a dot ornament; Turn Off is plain.
    fn row(self: &Rc<Self>, dev: &WiredDevice, row: &WiredRow, connectivity: u32) -> gtk::Button {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let label = |text: &str| {
            let l = gtk::Label::new(Some(text));
            l.set_xalign(0.0);
            l.set_hexpand(true);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l
        };
        let (name, action) = match row {
            WiredRow::AutoConnect { label: text } => {
                line.append(&gtk::Image::from_icon_name(dev.icon(connectivity)));
                line.append(&label(text));
                (text.clone(), Some(WiredAction::AutoConnect))
            }
            WiredRow::Single {
                label: text,
                active,
                action,
            } => {
                line.append(&gtk::Image::from_icon_name(dev.icon(connectivity)));
                line.append(&label(text));
                let subtitle =
                    gtk::Label::new(Some(if *active { "Disconnect" } else { "Connect" }));
                subtitle.add_css_class("device-subtitle");
                line.append(&subtitle);
                let name = if *active {
                    format!("Disconnect {text}")
                } else {
                    format!("Connect to {text}")
                };
                (name, Some(action.clone()))
            }
            WiredRow::Radio {
                label: text,
                active,
                action,
            } => {
                let dot = gtk::Image::from_icon_name(if *active {
                    "ornament-dot-checked-symbolic"
                } else {
                    "ornament-dot-unchecked-symbolic"
                });
                dot.add_css_class("popup-menu-ornament");
                line.append(&dot);
                line.append(&label(text));
                (text.clone(), action.clone())
            }
            WiredRow::TurnOff => {
                line.append(&label("Turn Off"));
                ("Turn Off".to_owned(), Some(WiredAction::Disconnect))
            }
        };
        let button = gtk::Button::builder().child(&line).build();
        button.add_css_class("qs-menu-item");
        button.add_css_class("nm-wired-item");
        button.update_property(&[gtk::accessible::Property::Label(&name)]);
        if let WiredRow::Radio { active, .. } = row {
            button.update_state(&[gtk::accessible::State::Checked(if *active {
                gtk::AccessibleTristate::True
            } else {
                gtk::AccessibleTristate::False
            })]);
        }
        let weak = Rc::downgrade(self);
        let device = dev.path.clone();
        button.connect_clicked(move |b| {
            // Picking an item closes the panel (PopupMenu activation).
            if let Some(popover) = b
                .ancestor(gtk::Popover::static_type())
                .and_downcast::<gtk::Popover>()
            {
                popover.popdown();
            }
            if let (Some(menu), Some(action)) = (weak.upgrade(), action.as_ref()) {
                menu.run(&device, action);
            }
        });
        button
    }

    fn sync_tile(&self) {
        let Some(tile) = self.tile.borrow().clone() else {
            return;
        };
        let t = logic::wired_toggle(&self.devices.borrow(), self.connectivity.get());
        tile.present(t.visible);
        tile.show_state(t.checked, t.subtitle.as_deref());
        tile.icon.set_icon_name(Some(t.icon));
    }

    /// Ask NetworkManager for `action` on `device`.
    fn run(&self, device: &str, action: &WiredAction) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let path = |p: &str| glib::variant::ObjectPath::try_from(p.to_owned()).ok();
        let (Some(dev), Some(none)) = (path(device), path("/")) else {
            return;
        };
        let (object, iface, method, args) = match action {
            WiredAction::Disconnect => (device.to_owned(), DEVICE, "Disconnect", None),
            WiredAction::Activate(saved) => {
                let Some(saved) = path(saved) else { return };
                (
                    NM_PATH.to_owned(),
                    NM,
                    "ActivateConnection",
                    Some((saved, dev, none).to_variant()),
                )
            }
            WiredAction::AutoConnect => {
                let empty: std::collections::HashMap<
                    String,
                    std::collections::HashMap<String, glib::Variant>,
                > = Default::default();
                (
                    NM_PATH.to_owned(),
                    NM,
                    "AddAndActivateConnection",
                    Some((empty, dev, none).to_variant()),
                )
            }
        };
        conn.call(
            Some(NM),
            &object,
            iface,
            method,
            args.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
}

/// Read one device; an ethernet one joins `found` with its profiles
/// and active connection.
fn read_device(
    conn: &gio::DBusConnection,
    path: String,
    primary: Option<String>,
    found: Rc<RefCell<Vec<WiredDevice>>>,
    done: Rc<Countdown>,
) {
    let conn2 = conn.clone();
    get_all(conn, &path.clone(), DEVICE, move |props| {
        let Some(props) = props else {
            done.tick();
            return;
        };
        let u = |k: &str| {
            props
                .lookup_value(k, None)
                .and_then(|v| v.get::<u32>())
                .unwrap_or(0)
        };
        if u("DeviceType") != ETHERNET {
            done.tick();
            return;
        }
        let profiles: Vec<String> = props
            .lookup_value("AvailableConnections", None)
            .and_then(|v| v.get::<Vec<glib::variant::ObjectPath>>())
            .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
            .unwrap_or_default();
        let active = object_path(&props, "ActiveConnection");
        let device = Rc::new(RefCell::new(WiredDevice {
            primary: active.is_some() && active == primary,
            path,
            state: u("State"),
            ..WiredDevice::default()
        }));
        let finish = Countdown::new(profiles.len() + 2, {
            let device = device.clone();
            move || {
                found.borrow_mut().push(device.take());
                done.tick();
            }
        });
        for profile in profiles {
            let (device, finish) = (device.clone(), finish.clone());
            let p = profile.clone();
            conn2.call(
                Some(NM),
                &profile,
                CONNECTION,
                "GetSettings",
                None,
                glib::VariantTy::new("(a{sa{sv}})").ok(),
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
                move |reply| {
                    // a{sa{sv}}: the "connection" section's id and
                    // timestamp.
                    let section = reply.ok().and_then(|v| {
                        v.child_value(0).iter().find_map(|entry| {
                            (entry.child_value(0).str() == Some("connection"))
                                .then(|| glib::VariantDict::new(Some(&entry.child_value(1))))
                        })
                    });
                    if let Some(section) = section {
                        let name = section
                            .lookup_value("id", None)
                            .and_then(|v| v.get::<String>());
                        if let Some(name) = name {
                            device.borrow_mut().connections.push(WiredConnection {
                                path: p,
                                name,
                                timestamp: section
                                    .lookup_value("timestamp", None)
                                    .and_then(|v| v.get::<u64>())
                                    .unwrap_or(0),
                            });
                        }
                    }
                    finish.tick();
                },
            );
        }
        match active {
            Some(active) => {
                let (device, finish) = (device.clone(), finish.clone());
                get_all(&conn2, &active, ACTIVE, move |props| {
                    if let Some(props) = props {
                        let profile = object_path(&props, "Connection").unwrap_or_default();
                        let state = props
                            .lookup_value("State", None)
                            .and_then(|v| v.get::<u32>())
                            .unwrap_or(0);
                        device.borrow_mut().active = Some((profile, state));
                    }
                    finish.tick();
                });
            }
            None => finish.tick(),
        }
        finish.tick();
    });
}

/// Runs its closure once `n` ticks have come in.
struct Countdown {
    left: Cell<usize>,
    then: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl Countdown {
    fn new(n: usize, then: impl FnOnce() + 'static) -> Rc<Self> {
        Rc::new(Self {
            left: Cell::new(n),
            then: RefCell::new(Some(Box::new(then))),
        })
    }

    fn tick(&self) {
        let left = self.left.get().saturating_sub(1);
        self.left.set(left);
        if left == 0 {
            if let Some(then) = self.then.borrow_mut().take() {
                then();
            }
        }
    }
}

/// An object-path property, `None` for "/" or absent.
fn object_path(d: &glib::VariantDict, key: &str) -> Option<String> {
    d.lookup_value(key, None)
        .and_then(|v| v.get::<glib::variant::ObjectPath>())
        .map(|p| p.as_str().to_owned())
        .filter(|p| p != "/")
}

/// `org.freedesktop.DBus.Properties.GetAll` as a dict, or `None`.
fn get_all(
    conn: &gio::DBusConnection,
    path: &str,
    iface: &str,
    done: impl FnOnce(Option<glib::VariantDict>) + 'static,
) {
    conn.call(
        Some(NM),
        path,
        PROPS,
        "GetAll",
        Some(&(iface,).to_variant()),
        glib::VariantTy::new("(a{sv})").ok(),
        gio::DBusCallFlags::NONE,
        5000,
        None::<&gio::Cancellable>,
        move |reply| {
            done(
                reply
                    .ok()
                    .map(|v| glib::VariantDict::new(Some(&v.child_value(0)))),
            )
        },
    );
}
