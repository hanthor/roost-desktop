//! GNOME's Bluetooth menu (bluetooth.js `BluetoothToggle`): the paired
//! or trusted devices BlueZ knows, connected first, each row toggling
//! its connection; a placeholder when there are none or the adapter is
//! off. The toggle's subtitle names the connected device(s).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::logic::{self, BtDevice};
use crate::services::Tile;

const BLUEZ: &str = "org.bluez";
const DEVICE: &str = "org.bluez.Device1";

/// The Bluetooth tile's menu and the state behind it.
pub struct BtMenu {
    list: gtk::Box,
    placeholder: gtk::Label,
    tile: RefCell<Option<Tile>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    powered: Cell<bool>,
    devices: RefCell<Vec<BtDevice>>,
    pending: Cell<bool>,
}

impl BtMenu {
    /// Rows go into `list`; `placeholder` shows when it is empty.
    pub fn new(list: gtk::Box, placeholder: gtk::Label) -> Rc<Self> {
        let menu = Rc::new(Self {
            list,
            placeholder,
            tile: RefCell::default(),
            conn: RefCell::default(),
            powered: Cell::new(false),
            devices: RefCell::default(),
            pending: Cell::new(false),
        });
        menu.render();
        menu
    }

    pub fn set_tile(&self, tile: Tile) {
        *self.tile.borrow_mut() = Some(tile);
    }

    /// BlueZ on `conn`: follow its devices.
    pub fn attach(self: &Rc<Self>, conn: &gio::DBusConnection) {
        let first = self.conn.borrow().is_none();
        *self.conn.borrow_mut() = Some(conn.clone());
        if first {
            for (iface, member) in [
                ("org.freedesktop.DBus.Properties", "PropertiesChanged"),
                ("org.freedesktop.DBus.ObjectManager", "InterfacesAdded"),
                ("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved"),
            ] {
                let weak = Rc::downgrade(self);
                let sub = conn.subscribe_to_signal(
                    Some(BLUEZ),
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

    /// The adapter's Powered state.
    pub fn set_powered(self: &Rc<Self>, on: bool) {
        self.powered.set(on);
        self.render();
    }

    fn schedule_reload(self: &Rc<Self>) {
        if self.pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            if let Some(menu) = weak.upgrade() {
                menu.pending.set(false);
                menu.reload();
            }
        });
    }

    fn reload(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        conn.call(
            Some(BLUEZ),
            "/",
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
            None,
            glib::VariantTy::new("(a{oa{sa{sv}}})").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                let devices: Vec<BtDevice> = reply
                    .ok()
                    .map(|v| {
                        v.child_value(0)
                            .iter()
                            .filter_map(|entry| {
                                let path = entry.child_value(0).str()?.to_owned();
                                let props = entry.child_value(1).iter().find_map(|i| {
                                    (i.child_value(0).str() == Some(DEVICE))
                                        .then(|| glib::VariantDict::new(Some(&i.child_value(1))))
                                })?;
                                let flag = |k: &str| {
                                    props
                                        .lookup_value(k, None)
                                        .and_then(|v| v.get::<bool>())
                                        .unwrap_or(false)
                                };
                                let text = |k: &str| {
                                    props.lookup_value(k, None).and_then(|v| v.get::<String>())
                                };
                                Some(BtDevice {
                                    alias: text("Alias")
                                        .or_else(|| text("Name"))
                                        .unwrap_or_else(|| path.clone()),
                                    icon: text("Icon"),
                                    paired: flag("Paired"),
                                    trusted: flag("Trusted"),
                                    connected: flag("Connected"),
                                    path,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                *menu.devices.borrow_mut() = devices;
                menu.render();
            },
        );
    }

    fn render(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let on = self.powered.get();
        let shown = logic::bt_devices(&self.devices.borrow(), on);
        for dev in &shown {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&gtk::Image::from_icon_name(&logic::bt_icon(
                dev.icon.as_deref(),
            )));
            let label = gtk::Label::new(Some(&dev.alias));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            row.append(&label);
            let subtitle = gtk::Label::new(Some(if dev.connected {
                "Disconnect"
            } else {
                "Connect"
            }));
            subtitle.add_css_class("device-subtitle");
            row.append(&subtitle);
            let button = gtk::Button::builder().child(&row).build();
            button.add_css_class("qs-menu-item");
            button.add_css_class("bt-device-item");
            button.update_property(&[gtk::accessible::Property::Label(&if dev.connected {
                format!("Disconnect {}", dev.alias)
            } else {
                format!("Connect to {}", dev.alias)
            })]);
            let weak = Rc::downgrade(self);
            let dev2 = dev.clone();
            button.connect_clicked(move |_| {
                if let Some(menu) = weak.upgrade() {
                    menu.toggle(&dev2);
                }
            });
            self.list.append(&button);
        }
        self.list.set_visible(!shown.is_empty());
        self.placeholder.set_visible(shown.is_empty());
        self.placeholder.set_text(if on {
            "No available or connected devices"
        } else {
            "Turn on Bluetooth to connect to devices"
        });
        if let Some(tile) = self.tile.borrow().clone() {
            tile.show_state(on, logic::bt_subtitle(&shown).as_deref());
            tile.icon.set_icon_name(Some(if on {
                "bluetooth-active-symbolic"
            } else {
                "bluetooth-disabled-symbolic"
            }));
        }
    }

    /// Connect or disconnect, as gnome-bluetooth's connect_service does
    /// (Device1.Connect / Disconnect).
    fn toggle(&self, dev: &BtDevice) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        conn.call(
            Some(BLUEZ),
            &dev.path,
            DEVICE,
            if dev.connected {
                "Disconnect"
            } else {
                "Connect"
            },
            None,
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
}
