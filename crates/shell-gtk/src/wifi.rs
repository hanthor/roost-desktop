//! GNOME's Wi-Fi menu (network.js `NMWirelessToggle`): the networks
//! NetworkManager sees, grouped and sorted as GNOME does, scanned when
//! the menu opens and every 15 s while it stays open. A saved network
//! activates its connection; an open one gets a new connection; one
//! that needs a password or 802.1X goes to Settings' Wi-Fi panel. The
//! toggle shows the connected network's name and signal.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::logic::{self, AccessPoint, WifiNetwork, WifiSecurity};
use crate::services::Tile;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
/// GNOME rescans this often while the menu is open (`WIFI_SCAN_FREQUENCY`).
const SCAN_SECONDS: u32 = 15;

#[derive(Default)]
struct Seen {
    aps: Vec<AccessPoint>,
    active: Option<String>,
    known: Vec<(Vec<u8>, String)>,
}

/// The Wi-Fi tile's menu and the state behind it.
pub struct WifiMenu {
    list: gtk::Box,
    tile: RefCell<Option<Tile>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    device: RefCell<Option<String>>,
    networks: RefCell<Vec<WifiNetwork>>,
    pending: Cell<bool>,
    scan: RefCell<Option<glib::SourceId>>,
    enabled: Cell<bool>,
}

impl WifiMenu {
    /// The menu's network list goes into `list`.
    pub fn new(list: gtk::Box) -> Rc<Self> {
        Rc::new(Self {
            list,
            tile: RefCell::default(),
            conn: RefCell::default(),
            device: RefCell::default(),
            networks: RefCell::default(),
            pending: Cell::new(false),
            scan: RefCell::default(),
            enabled: Cell::new(false),
        })
    }

    /// The toggle whose subtitle and icon follow the connection.
    pub fn set_tile(&self, tile: Tile) {
        *self.tile.borrow_mut() = Some(tile);
    }

    /// NetworkManager's Wi-Fi device (or none), on `conn`.
    pub fn attach(self: &Rc<Self>, conn: &gio::DBusConnection, device: Option<String>) {
        let first = self.conn.borrow().is_none();
        *self.conn.borrow_mut() = Some(conn.clone());
        *self.device.borrow_mut() = device;
        if first {
            // Any NetworkManager property change or access point coming
            // or going: reload (debounced).
            for (iface, member) in [
                (PROPS, "PropertiesChanged"),
                (WIRELESS, "AccessPointAdded"),
                (WIRELESS, "AccessPointRemoved"),
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

    /// WirelessEnabled, as the tile shows it.
    pub fn set_enabled(self: &Rc<Self>, on: bool) {
        self.enabled.set(on);
        self.sync_tile();
    }

    /// The menu opened (scan now and every 15 s) or closed.
    pub fn set_open(self: &Rc<Self>, open: bool) {
        if let Some(id) = self.scan.borrow_mut().take() {
            id.remove();
        }
        if !open {
            return;
        }
        self.request_scan();
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local(SCAN_SECONDS, move || match weak.upgrade() {
            Some(menu) => {
                menu.request_scan();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        *self.scan.borrow_mut() = Some(id);
    }

    fn request_scan(&self) {
        let (Some(conn), Some(device)) = (self.conn.borrow().clone(), self.device.borrow().clone())
        else {
            return;
        };
        if !self.enabled.get() {
            return;
        }
        let options = glib::VariantDict::new(None).end();
        conn.call(
            Some(NM),
            &device,
            WIRELESS,
            "RequestScan",
            Some(&glib::Variant::tuple_from_iter([options])),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            |_| {},
        );
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

    /// Read the device's access points, the active one and the saved
    /// connections it can use, then rebuild.
    fn reload(self: &Rc<Self>) {
        let (Some(conn), Some(device)) = (self.conn.borrow().clone(), self.device.borrow().clone())
        else {
            self.show(Vec::new());
            return;
        };
        let weak = Rc::downgrade(self);
        let conn2 = conn.clone();
        let device_path = device.clone();
        get_all(&conn, &device_path, WIRELESS, move |props| {
            let Some(props) = props else { return };
            let paths = |key: &str| -> Vec<String> {
                props
                    .lookup_value(key, None)
                    .and_then(|v| v.get::<Vec<glib::variant::ObjectPath>>())
                    .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
                    .unwrap_or_default()
            };
            let ap_paths = paths("AccessPoints");
            let active = props
                .lookup_value("ActiveAccessPoint", None)
                .and_then(|v| v.get::<glib::variant::ObjectPath>())
                .map(|p| p.as_str().to_owned())
                .filter(|p| p != "/");
            let seen = Rc::new(RefCell::new(Seen {
                active,
                ..Seen::default()
            }));
            let conn3 = conn2.clone();
            let device2 = device.clone();
            get_all(&conn2, &device2, DEVICE, move |dev| {
                let conns: Vec<String> = dev
                    .and_then(|d| d.lookup_value("AvailableConnections", None))
                    .and_then(|v| v.get::<Vec<glib::variant::ObjectPath>>())
                    .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
                    .unwrap_or_default();
                let left = Rc::new(Cell::new(ap_paths.len() + conns.len() + 1));
                let done = {
                    let (seen, left, weak) = (seen.clone(), left.clone(), weak.clone());
                    Rc::new(move || {
                        left.set(left.get() - 1);
                        if left.get() == 0 {
                            if let Some(menu) = weak.upgrade() {
                                let s = seen.borrow();
                                menu.show(logic::wifi_networks(
                                    &s.aps,
                                    s.active.as_deref(),
                                    &s.known,
                                ));
                            }
                        }
                    })
                };
                for path in ap_paths {
                    let (seen, done) = (seen.clone(), done.clone());
                    let p = path.clone();
                    get_all(&conn3, &path, AP, move |props| {
                        if let Some(props) = props {
                            let u = |k: &str| {
                                props
                                    .lookup_value(k, None)
                                    .and_then(|v| v.get::<u32>())
                                    .unwrap_or(0)
                            };
                            seen.borrow_mut().aps.push(AccessPoint {
                                path: p,
                                ssid: props
                                    .lookup_value("Ssid", None)
                                    .and_then(|v| v.get::<Vec<u8>>())
                                    .unwrap_or_default(),
                                strength: props
                                    .lookup_value("Strength", None)
                                    .and_then(|v| v.get::<u8>())
                                    .unwrap_or(0),
                                flags: u("Flags"),
                                wpa_flags: u("WpaFlags"),
                                rsn_flags: u("RsnFlags"),
                                mode: u("Mode"),
                            });
                        }
                        done();
                    });
                }
                for path in conns {
                    let (seen, done) = (seen.clone(), done.clone());
                    let p = path.clone();
                    conn3.call(
                        Some(NM),
                        &path,
                        CONNECTION,
                        "GetSettings",
                        None,
                        glib::VariantTy::new("(a{sa{sv}})").ok(),
                        gio::DBusCallFlags::NONE,
                        5000,
                        None::<&gio::Cancellable>,
                        move |reply| {
                            // a{sa{sv}}: find the 802-11-wireless
                            // section, then its ssid.
                            let ssid = reply.ok().and_then(|v| {
                                let settings = v.child_value(0);
                                let wifi = settings.iter().find_map(|entry| {
                                    (entry.child_value(0).str() == Some("802-11-wireless"))
                                        .then(|| entry.child_value(1))
                                })?;
                                glib::VariantDict::new(Some(&wifi))
                                    .lookup_value("ssid", None)?
                                    .get::<Vec<u8>>()
                            });
                            if let Some(ssid) = ssid {
                                seen.borrow_mut().known.push((ssid, p));
                            }
                            done();
                        },
                    );
                }
                done();
            });
        });
    }

    fn show(self: &Rc<Self>, networks: Vec<WifiNetwork>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for net in &networks {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let icons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            icons.append(&gtk::Image::from_icon_name(logic::signal_icon(
                net.strength,
            )));
            if net.security.secure() {
                let lock = gtk::Image::from_icon_name("network-wireless-encrypted-symbolic");
                lock.add_css_class("wireless-secure-icon");
                // `.nm-network-item .wireless-secure-icon { icon-size:
                // 0.5455em }`: 8px at the 11pt menu font.
                lock.set_pixel_size(8);
                lock.set_valign(gtk::Align::End);
                icons.append(&lock);
            }
            row.append(&icons);
            let label = gtk::Label::new(Some(&net.name));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&label);
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.set_visible(net.active);
            row.append(&check);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("qs-menu-item");
            button.add_css_class("nm-network-item");
            // GNOME's accessible name: "<name>, Secure|Not secure,
            // Signal strength N%".
            button.update_property(&[gtk::accessible::Property::Label(&format!(
                "{}, {}, Signal strength {}%",
                net.name,
                if net.security.secure() {
                    "Secure"
                } else {
                    "Not secure"
                },
                net.strength
            ))]);
            let weak = Rc::downgrade(self);
            let net2 = net.clone();
            button.connect_clicked(move |_| {
                if let Some(menu) = weak.upgrade() {
                    menu.activate(&net2);
                }
            });
            self.list.append(&button);
        }
        *self.networks.borrow_mut() = networks;
        self.sync_tile();
    }

    /// GNOME's subtitle (the connected network's name, else none) and
    /// icon (its signal, or disabled / none).
    fn sync_tile(&self) {
        let Some(tile) = self.tile.borrow().clone() else {
            return;
        };
        let networks = self.networks.borrow();
        let active = networks.iter().find(|n| n.active);
        let on = self.enabled.get();
        tile.show_state(on, active.filter(|_| on).map(|n| n.name.as_str()));
        tile.icon.set_icon_name(Some(match (on, active) {
            (false, _) => "network-wireless-disabled-symbolic",
            (true, Some(net)) => logic::signal_icon(net.strength),
            (true, None) => "network-wireless-signal-none-symbolic",
        }));
    }

    /// A saved network activates its connection; an open one gets a
    /// new connection; anything needing secrets goes to Settings (GNOME
    /// asks for a password through its own agent).
    fn activate(&self, net: &WifiNetwork) {
        let (Some(conn), Some(device)) = (self.conn.borrow().clone(), self.device.borrow().clone())
        else {
            return;
        };
        let path = |p: &str| glib::variant::ObjectPath::try_from(p.to_owned()).ok();
        let (Some(dev), Some(ap)) = (path(&device), path(&net.ap)) else {
            return;
        };
        match (&net.connection, net.security) {
            (Some(saved), _) => {
                let (Some(saved), Some(none)) = (path(saved), path("/")) else {
                    return;
                };
                conn.call(
                    Some(NM),
                    NM_PATH,
                    NM,
                    "ActivateConnection",
                    Some(&(saved, dev, none).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
            (None, WifiSecurity::Open) => {
                let empty: std::collections::HashMap<
                    String,
                    std::collections::HashMap<String, glib::Variant>,
                > = Default::default();
                conn.call(
                    Some(NM),
                    NM_PATH,
                    NM,
                    "AddAndActivateConnection",
                    Some(&(empty, dev, ap).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    None::<&gio::Cancellable>,
                    |_| {},
                );
            }
            (None, WifiSecurity::Enterprise) => {
                let _ = std::process::Command::new("gnome-control-center")
                    .args(["wifi", "connect-8021x-wifi", &device, &net.ap])
                    .spawn();
            }
            (None, WifiSecurity::Personal) => {
                let _ = std::process::Command::new("gnome-control-center")
                    .arg("wifi")
                    .spawn();
            }
        }
    }
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
