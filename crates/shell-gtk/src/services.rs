//! Quick-settings services (#55): the daemons behind GNOME 51's grid.
//!
//! Every caller is asynchronous on the GLib main loop and bounded by a
//! call timeout, so an absent or stuck daemon can never hang the shell.
//! A tile whose daemon is not on the bus is hidden, as GNOME hides it.
//!
//! - Wi-Fi and Wired: NetworkManager (system bus).
//! - Bluetooth: BlueZ's first adapter (system bus).
//! - Power Mode: power-profiles-daemon, under its UPower name or the
//!   older `net.hadess` one (system bus).
//! - Brightness: the first sysfs backlight, written through logind's
//!   `SetBrightness` so the shell needs no root.
//! - Volume: `wpctl` against the default PipeWire sink.
//!
//! Night Light and Dark Style are plain GSettings keys and live with
//! the grid in `main.rs`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::prelude::*;

use crate::logic;

/// Bound on every D-Bus round trip (ms).
const CALL_TIMEOUT_MS: i32 = 2000;
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

const NM_NAME: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
/// NetworkManager device types.
const NM_DEVICE_ETHERNET: u32 = 1;
const NM_DEVICE_WIFI: u32 = 2;
/// NM_ACTIVE_CONNECTION_STATE / NM_DEVICE_STATE_ACTIVATED.
const NM_DEVICE_ACTIVATED: u32 = 100;

const BLUEZ_NAME: &str = "org.bluez";
const BLUEZ_ADAPTER_IFACE: &str = "org.bluez.Adapter1";

/// power-profiles-daemon names, newest first.
const PPD: [(&str, &str, &str); 2] = [
    (
        "org.freedesktop.UPower.PowerProfiles",
        "/org/freedesktop/UPower/PowerProfiles",
        "org.freedesktop.UPower.PowerProfiles",
    ),
    (
        "net.hadess.PowerProfiles",
        "/net/hadess/PowerProfiles",
        "net.hadess.PowerProfiles",
    ),
];

const LOGIND_NAME: &str = "org.freedesktop.login1";
const LOGIND_SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const LOGIND_SESSION_IFACE: &str = "org.freedesktop.login1.Session";

/// One quick-settings toggle: the button plus its subtitle line.
#[derive(Clone)]
pub struct Tile {
    pub button: gtk::ToggleButton,
    pub subtitle: gtk::Label,
    /// The toggle's icon (GNOME swaps it with state, e.g. power profile).
    pub icon: gtk::Image,
    /// What the grid places and hides: the button, or the button with
    /// its menu arrow.
    pub outer: gtk::Widget,
    /// Set while the shell itself updates the button from daemon
    /// state, so the toggled handler does not echo it back.
    syncing: Rc<Cell<bool>>,
}

impl Tile {
    pub fn new(button: gtk::ToggleButton, subtitle: gtk::Label, icon: gtk::Image) -> Self {
        Self {
            outer: button.clone().upcast(),
            button,
            subtitle,
            icon,
            syncing: Rc::new(Cell::new(false)),
        }
    }

    /// Reflect daemon state without firing the user handler.
    pub fn show_state(&self, active: bool, subtitle: Option<&str>) {
        self.syncing.set(true);
        self.button.set_active(active);
        self.syncing.set(false);
        match subtitle {
            Some(text) => {
                self.subtitle.set_text(text);
                self.subtitle.set_visible(true);
            }
            None => self.subtitle.set_visible(false),
        }
    }

    /// Hide or show the whole tile; the grid closes the gap.
    pub fn present(&self, present: bool) {
        self.outer.set_visible(present);
    }

    /// Run `f` on user toggles only.
    pub fn on_user_toggle(&self, f: impl Fn(bool) + 'static) {
        let syncing = self.syncing.clone();
        self.button.connect_toggled(move |b| {
            if !syncing.get() {
                f(b.is_active());
            }
        });
    }
}

/// An object on a bus, reached by asynchronous calls only.
#[derive(Clone)]
struct Remote {
    conn: gio::DBusConnection,
    name: String,
    path: String,
    iface: String,
}

impl Remote {
    fn new(conn: &gio::DBusConnection, name: &str, path: &str, iface: &str) -> Self {
        Self {
            conn: conn.clone(),
            name: name.to_owned(),
            path: path.to_owned(),
            iface: iface.to_owned(),
        }
    }

    /// Call a method. With `reply`, GDBus checks the reply's type, so a
    /// misbehaving daemon reads as no answer instead of a crash.
    fn call(
        &self,
        iface: &str,
        method: &str,
        args: Option<glib::Variant>,
        reply: Option<&str>,
        done: impl FnOnce(Option<glib::Variant>) + 'static,
    ) {
        let reply = reply.and_then(|r| glib::VariantTy::new(r).ok());
        self.conn.call(
            Some(&self.name),
            &self.path,
            iface,
            method,
            args.as_ref(),
            reply,
            gio::DBusCallFlags::NO_AUTO_START,
            CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
            move |reply| done(reply.ok()),
        );
    }

    /// Every property as an `a{sv}` dict.
    fn get_all(&self, done: impl FnOnce(Option<glib::VariantDict>) + 'static) {
        let args = (self.iface.as_str(),).to_variant();
        self.call(
            PROPS_IFACE,
            "GetAll",
            Some(args),
            Some("(a{sv})"),
            move |reply| done(reply.map(|v| glib::VariantDict::new(Some(&v.child_value(0))))),
        );
    }

    fn set(&self, prop: &str, value: glib::Variant) {
        // A Variant tuple member already serializes as `v`.
        let args = (self.iface.as_str(), prop, value).to_variant();
        self.call(PROPS_IFACE, "Set", Some(args), None, |_| {});
    }

    /// Re-read every property whenever the object announces a change.
    fn watch(&self, changed: impl Fn() + 'static) {
        let iface = self.iface.clone();
        let sub = self.conn.subscribe_to_signal(
            Some(&self.name),
            Some(PROPS_IFACE),
            Some("PropertiesChanged"),
            Some(&self.path),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let source = signal.parameters.try_child_value(0);
                if source.as_ref().and_then(|v| v.str()) == Some(iface.as_str()) {
                    changed();
                }
            },
        );
        // The subscription lives as long as the shell.
        std::mem::forget(sub);
    }
}

/// Call `f(true)` when `name` gains an owner and `f(false)` when it
/// loses one, starting with the current state.
pub fn watch_name(conn: &gio::DBusConnection, name: &str, f: impl Fn(bool) + 'static) {
    let f = Rc::new(f);
    let sub = {
        let f = f.clone();
        conn.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            Some(name),
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let new_owner = signal.parameters.try_child_value(2);
                f(new_owner
                    .as_ref()
                    .and_then(|v| v.str())
                    .is_some_and(|s| !s.is_empty()));
            },
        )
    };
    std::mem::forget(sub);
    conn.call(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(name,).to_variant()),
        Some(glib::VariantTy::new("(b)").expect("valid type")),
        gio::DBusCallFlags::NONE,
        CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
        move |reply| {
            let owned = reply
                .ok()
                .and_then(|v| v.child_value(0).get::<bool>())
                .unwrap_or(false);
            f(owned);
        },
    );
}

fn dict_bool(d: &glib::VariantDict, key: &str) -> Option<bool> {
    d.lookup_value(key, None).and_then(|v| v.get::<bool>())
}

fn dict_string(d: &glib::VariantDict, key: &str) -> Option<String> {
    d.lookup_value(key, None).and_then(|v| v.get::<String>())
}

/// The tiles and sliders this module drives.
pub struct Widgets {
    pub wifi: Tile,
    /// The Wi-Fi tile's network menu.
    pub wifi_menu: Rc<crate::wifi::WifiMenu>,
    /// The wired tile's profile menu; it drives the tile too.
    pub wired_menu: Rc<crate::wired::WiredMenu>,
    /// The Bluetooth tile's device menu.
    pub bt_menu: Rc<crate::bt_menu::BtMenu>,
    pub wired: Tile,
    pub bluetooth: Tile,
    pub power_mode: Tile,
    /// The Power Mode menu: its header icon and one item per profile
    /// (profile name, button, check ornament).
    pub power_header: gtk::Image,
    pub power_items: Vec<(&'static str, gtk::Button, gtk::Image)>,
    /// Panel status icons GNOME shows only while they mean something
    /// (panel.js `_indicators`): network, volume, power profile.
    pub panel_network: gtk::Image,
    pub panel_volume: gtk::Image,
    pub panel_power_profile: gtk::Image,
    pub panel_battery: gtk::Box,
    pub battery_icon: gtk::Image,
    pub battery_percentage: gtk::Label,
    pub battery_summary: gtk::Label,
    /// The microphone: the panel's privacy indicator and the slider row.
    pub panel_mic: gtk::Image,
    pub mic: gtk::Scale,
    pub mic_mute: gtk::Button,
    pub mic_row: gtk::Box,
    pub volume: gtk::Scale,
    pub mute: gtk::Button,
    pub brightness_row: gtk::Box,
    /// The sound output menu's rows and the slider's arrow opening it.
    pub sound_list: gtk::Box,
    pub sound_arrow: gtk::ToggleButton,
    pub brightness: gtk::Scale,
}

/// Wire every service to its widgets. Missing buses leave the tiles
/// hidden; nothing here blocks.
pub fn attach(w: &Rc<Widgets>) {
    for tile in [&w.wifi, &w.wired, &w.bluetooth, &w.power_mode] {
        tile.present(false);
    }
    // The system bus connection itself is asynchronous too.
    let w2 = w.clone();
    gio::bus_get(
        gio::BusType::System,
        None::<&gio::Cancellable>,
        move |conn| match conn {
            Ok(conn) => {
                network(&conn, &w2);
                bluetooth(&conn, &w2);
                power_profiles(&conn, &w2, 0);
                brightness(&conn, &w2);
                battery(&conn, &w2);
            }
            Err(e) => eprintln!("tuna-shell-gtk: no system bus, service tiles hidden: {e}"),
        },
    );
    audio(w);
}

fn network(conn: &gio::DBusConnection, w: &Rc<Widgets>) {
    let nm = Remote::new(conn, NM_NAME, NM_PATH, NM_NAME);
    let refresh: Rc<dyn Fn()> = {
        let (nm, w) = (nm.clone(), w.clone());
        let conn = conn.clone();
        Rc::new(move || {
            let w1 = w.clone();
            nm.get_all(move |props| {
                let Some(props) = props else {
                    w1.wifi.present(false);
                    return;
                };
                let on = dict_bool(&props, "WirelessEnabled").unwrap_or(false);
                // GNOME's subtitle is the connected network's name.
                w1.wifi_menu.set_enabled(on);
            });
            // Which tiles exist depends on the devices present.
            let (w, conn) = (w.clone(), conn.clone());
            nm.call(NM_NAME, "GetDevices", None, Some("(ao)"), move |reply| {
                let paths: Vec<String> = reply
                    .and_then(|v| v.child_value(0).get::<Vec<glib::variant::ObjectPath>>())
                    .map(|p| p.into_iter().map(|p| p.as_str().to_owned()).collect())
                    .unwrap_or_default();
                let seen = Rc::new(RefCell::new((false, false, false)));
                let wifi_device: Rc<RefCell<Option<String>>> = Rc::default();
                let left = Rc::new(Cell::new(paths.len()));
                if paths.is_empty() {
                    w.wifi.present(false);
                }
                for path in paths {
                    let dev = Remote::new(&conn, NM_NAME, &path, NM_DEVICE_IFACE);
                    let (w, seen, left) = (w.clone(), seen.clone(), left.clone());
                    let (wifi_device, conn, path) =
                        (wifi_device.clone(), conn.clone(), path.clone());
                    dev.get_all(move |props| {
                        if let Some(props) = props {
                            let kind = props
                                .lookup_value("DeviceType", None)
                                .and_then(|v| v.get::<u32>());
                            let state = props
                                .lookup_value("State", None)
                                .and_then(|v| v.get::<u32>())
                                .unwrap_or(0);
                            let mut s = seen.borrow_mut();
                            match kind {
                                Some(NM_DEVICE_WIFI) => {
                                    s.0 = true;
                                    wifi_device.borrow_mut().get_or_insert(path);
                                }
                                Some(NM_DEVICE_ETHERNET) => {
                                    s.1 = true;
                                    s.2 |= state == NM_DEVICE_ACTIVATED;
                                }
                                _ => {}
                            }
                        }
                        left.set(left.get() - 1);
                        if left.get() == 0 {
                            let (wifi, wired, connected) = *seen.borrow();
                            w.wifi.present(wifi);
                            w.wifi_menu.attach(&conn, wifi_device.borrow().clone());
                            w.wired_menu.attach(&conn);
                            // GNOME's primary network indicator: shown
                            // while the main connection is up.
                            w.panel_network.set_visible(wired && connected);
                        }
                    });
                }
            });
        })
    };
    {
        let refresh = refresh.clone();
        nm.watch(move || refresh());
    }
    {
        let w = w.clone();
        watch_name(conn, NM_NAME, move |owned| {
            if owned {
                refresh();
            } else {
                w.wifi.present(false);
                w.wired_menu.detach();
                w.panel_network.set_visible(false);
            }
        });
    }
    {
        let nm = nm.clone();
        w.wifi
            .on_user_toggle(move |on| nm.set("WirelessEnabled", on.to_variant()));
    }
}

fn bluetooth(conn: &gio::DBusConnection, w: &Rc<Widgets>) {
    let adapter: Rc<RefCell<Option<Remote>>> = Rc::new(RefCell::new(None));
    let refresh: Rc<dyn Fn()> = {
        let (conn, w, adapter) = (conn.clone(), w.clone(), adapter.clone());
        Rc::new(move || {
            let manager = Remote::new(&conn, BLUEZ_NAME, "/", "org.freedesktop.DBus.ObjectManager");
            let (conn, w, adapter) = (conn.clone(), w.clone(), adapter.clone());
            manager.call(
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
                None,
                Some("(a{oa{sa{sv}}})"),
                move |reply| {
                    let found = reply.and_then(|v| {
                        v.child_value(0).iter().find_map(|entry| {
                            let path = entry.child_value(0);
                            let ifaces = entry.child_value(1);
                            let props = ifaces.iter().find_map(|i| {
                                (i.child_value(0).str() == Some(BLUEZ_ADAPTER_IFACE))
                                    .then(|| i.child_value(1))
                            })?;
                            let powered = glib::VariantDict::new(Some(&props))
                                .lookup_value("Powered", None)
                                .and_then(|v| v.get::<bool>())
                                .unwrap_or(false);
                            Some((path.str()?.to_owned(), powered))
                        })
                    });
                    match found {
                        Some((path, powered)) => {
                            let fresh = adapter.borrow().as_ref().map(|a| a.path.clone())
                                != Some(path.clone());
                            if fresh {
                                let remote =
                                    Remote::new(&conn, BLUEZ_NAME, &path, BLUEZ_ADAPTER_IFACE);
                                let (w2, r2) = (w.clone(), remote.clone());
                                remote.watch(move || {
                                    let w3 = w2.clone();
                                    r2.get_all(move |p| {
                                        if let Some(on) = p.and_then(|p| dict_bool(&p, "Powered")) {
                                            w3.bt_menu.set_powered(on);
                                        }
                                    });
                                });
                                *adapter.borrow_mut() = Some(remote);
                            }
                            w.bluetooth.present(true);
                            // GNOME's subtitle names connected devices.
                            w.bt_menu.attach(&conn);
                            w.bt_menu.set_powered(powered);
                        }
                        None => w.bluetooth.present(false),
                    }
                },
            );
        })
    };
    {
        let w = w.clone();
        watch_name(conn, BLUEZ_NAME, move |owned| {
            if owned {
                refresh();
            } else {
                w.bluetooth.present(false);
            }
        });
    }
    w.bluetooth.on_user_toggle(move |on| {
        if let Some(a) = adapter.borrow().as_ref() {
            a.set("Powered", on.to_variant());
        }
    });
}

fn power_profiles(conn: &gio::DBusConnection, w: &Rc<Widgets>, which: usize) {
    let Some(&(name, path, iface)) = PPD.get(which) else {
        return;
    };
    let ppd = Remote::new(conn, name, path, iface);
    let profile = Rc::new(RefCell::new(String::from(logic::BALANCED)));
    let refresh: Rc<dyn Fn()> = {
        let (ppd, w, profile) = (ppd.clone(), w.clone(), profile.clone());
        Rc::new(move || {
            let (w, profile) = (w.clone(), profile.clone());
            ppd.get_all(move |props| {
                let Some(active) = props.and_then(|p| dict_string(&p, "ActiveProfile")) else {
                    return;
                };
                w.power_mode.present(true);
                w.power_mode.show_state(
                    logic::power_mode_checked(&active),
                    Some(logic::power_mode_label(&active)),
                );
                let icon = logic::power_mode_icon(&active);
                w.power_mode.icon.set_icon_name(Some(icon));
                // The panel shows the profile only when not balanced.
                w.panel_power_profile.set_icon_name(Some(icon));
                w.panel_power_profile
                    .set_visible(logic::power_mode_checked(&active));
                w.power_header.set_icon_name(Some(icon));
                if logic::power_mode_checked(&active) {
                    w.power_header.add_css_class("active");
                } else {
                    w.power_header.remove_css_class("active");
                }
                for (name, _, ornament) in &w.power_items {
                    ornament.set_visible(*name == active);
                }
                *profile.borrow_mut() = active;
            });
        })
    };
    {
        let refresh = refresh.clone();
        ppd.watch(move || refresh());
    }
    {
        let (w, conn) = (w.clone(), conn.clone());
        let tried_next = Rc::new(Cell::new(false));
        let owned_once = Rc::new(Cell::new(false));
        watch_name(&conn.clone(), name, move |owned| {
            if owned {
                owned_once.set(true);
                refresh();
            } else {
                if owned_once.get() {
                    w.power_mode.present(false);
                }
                // Fall back to the older name once if this one is absent.
                if !owned_once.get() && !tried_next.replace(true) {
                    power_profiles(&conn, &w, which + 1);
                }
            }
        });
    }
    for (name, button, _) in &w.power_items {
        let (ppd, name) = (ppd.clone(), *name);
        button.connect_clicked(move |_| ppd.set("ActiveProfile", name.to_variant()));
    }
    {
        let ppd = ppd.clone();
        w.power_mode.on_user_toggle(move |_| {
            let next = logic::power_mode_after_click(&profile.borrow());
            ppd.set("ActiveProfile", next.to_variant());
        });
    }
}

/// First sysfs backlight (`TUNA_BACKLIGHT_ROOT` overrides the root
/// for tests).
fn backlight() -> Option<(String, PathBuf)> {
    let root = std::env::var_os("TUNA_BACKLIGHT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/backlight"));
    backlight_for(&root, None)
}

/// Kernel backlights associated with a DRM connector have a cardN-CONNECTOR
/// ancestor (directly or via `device`). Never substitute another monitor.
fn backlight_for(root: &std::path::Path, output: Option<&str>) -> Option<(String, PathBuf)> {
    let mut entries: Vec<_> = std::fs::read_dir(root).ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let entry = entries.into_iter().find(|entry| {
        output.is_none_or(|output| {
            [entry.path(), entry.path().join("device")]
                .iter()
                .filter_map(|path| std::fs::canonicalize(path).ok())
                .any(|path| {
                    path.ancestors().any(|ancestor| {
                        ancestor
                            .file_name()
                            .and_then(|name| name.to_str())
                            .and_then(|name| name.strip_prefix("card"))
                            .and_then(|name| name.split_once('-'))
                            .is_some_and(|(card, connector)| {
                                !card.is_empty()
                                    && card.chars().all(|c| c.is_ascii_digit())
                                    && connector == output
                            })
                    })
                })
        })
    })?;
    Some((
        entry.file_name().to_string_lossy().into_owned(),
        entry.path(),
    ))
}

#[derive(Clone, Copy)]
pub enum BrightnessStep {
    Up,
    Down,
    Cycle,
}

/// GNOME's brightness keys (brightnessManager.js): step the backlight
/// by a twentieth through logind and return the new level (0..1) for
/// the OSD, or `None` without a backlight.
pub fn step_brightness_for(step: BrightnessStep, output: Option<&str>) -> Option<f64> {
    let root = std::env::var_os("TUNA_BACKLIGHT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/backlight"));
    let (name, dir) = backlight_for(&root, output)?;
    let max = read_u32(&dir.join("max_brightness"))?;
    let now = read_u32(&dir.join("brightness"))?;
    let current = logic::brightness_percent(now, max);
    let percent = match step {
        BrightnessStep::Up => logic::brightness_step(current, true),
        BrightnessStep::Down => logic::brightness_step(current, false),
        BrightnessStep::Cycle => {
            if current >= 100.0 {
                0.0
            } else {
                logic::brightness_step(current, true)
            }
        }
    };
    let value = logic::brightness_value(percent, max);
    let conn = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).ok()?;
    conn.call(
        Some(LOGIND_NAME),
        LOGIND_SESSION_PATH,
        LOGIND_SESSION_IFACE,
        "SetBrightness",
        Some(&("backlight", name.as_str(), value).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
        |_| {},
    );
    Some(percent / 100.0)
}

fn read_u32(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn brightness(conn: &gio::DBusConnection, w: &Rc<Widgets>) {
    let Some((name, dir)) = backlight() else {
        w.brightness_row.set_visible(false);
        return;
    };
    let Some(max) = read_u32(&dir.join("max_brightness")) else {
        w.brightness_row.set_visible(false);
        return;
    };
    w.brightness_row.set_visible(true);
    let syncing = Rc::new(Cell::new(false));
    let read = {
        let (w, dir, syncing) = (w.clone(), dir.clone(), syncing.clone());
        move || {
            if let Some(value) = read_u32(&dir.join("brightness")) {
                syncing.set(true);
                w.brightness
                    .set_value(logic::brightness_percent(value, max));
                syncing.set(false);
            }
        }
    };
    read();
    // Re-read whenever the menu opens: other tools change it too.
    w.brightness.connect_map(move |_| read());
    let session = Remote::new(conn, LOGIND_NAME, LOGIND_SESSION_PATH, LOGIND_SESSION_IFACE);
    w.brightness.connect_value_changed(move |s| {
        if syncing.get() {
            return;
        }
        let value = logic::brightness_value(s.value(), max);
        let args = ("backlight", name.as_str(), value).to_variant();
        session.call(
            LOGIND_SESSION_IFACE,
            "SetBrightness",
            Some(args),
            None,
            |_| {},
        );
    });
}

/// Run `wpctl` asynchronously; `done` gets stdout, or `None` when the
/// tool is missing or fails.
fn wpctl(args: &[&str], done: impl FnOnce(Option<String>) + 'static) {
    let mut argv: Vec<&std::ffi::OsStr> = vec![std::ffi::OsStr::new("wpctl")];
    argv.extend(args.iter().map(std::ffi::OsStr::new));
    let proc = match gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
    ) {
        Ok(p) => p,
        Err(_) => return done(None),
    };
    let weak = proc.downgrade();
    glib::timeout_add_seconds_local_once(2, move || {
        if let Some(proc) = weak.upgrade() {
            proc.force_exit();
        }
    });
    proc.communicate_utf8_async(None, None::<&gio::Cancellable>, move |res| {
        done(match res {
            Ok((out, _)) => out.map(|s| s.to_string()),
            Err(_) => None,
        })
    });
}

const SINK: &str = "@DEFAULT_AUDIO_SINK@";
const SOURCE: &str = "@DEFAULT_AUDIO_SOURCE@";

/// pw-dump --monitor delivers node, metadata and device-route events on
/// the GLib loop. Widget changes happen in that callback, before the next
/// frame, without a status/volume subprocess round trip or polling timer.
struct AudioUi {
    widgets: Rc<Widgets>,
    state: RefCell<crate::audio_state::AudioState>,
    last: RefCell<crate::audio_state::Snapshot>,
    syncing: Cell<bool>,
    process: RefCell<Option<gio::Subprocess>>,
    stopping: Cell<bool>,
    sound: Option<gio::Settings>,
}

impl AudioUi {
    fn refresh_volume_limit(&self) {
        let limit = crate::media_keys::output_limit(self.sound.as_ref());
        self.syncing.set(true);
        self.widgets.volume.clear_marks();
        self.widgets.volume.set_range(0.0, limit);
        if limit > 100.0 {
            self.widgets
                .volume
                .add_mark(100.0, gtk::PositionType::Bottom, None);
        }
        if let Some((percent, muted)) = self.last.borrow().sink {
            self.widgets
                .volume
                .set_value(if muted { 0.0 } else { percent });
        }
        self.syncing.set(false);
    }

    fn update(self: &Rc<Self>, snapshot: crate::audio_state::Snapshot) {
        if *self.last.borrow() == snapshot {
            return;
        }
        let w = &self.widgets;
        self.syncing.set(true);
        if let Some((percent, muted)) = snapshot.sink {
            if let Some(row) = w.volume.parent() {
                row.set_visible(true);
            }
            w.panel_volume
                .set_icon_name(Some(logic::volume_icon(percent, muted)));
            w.panel_volume.set_visible(true);
            w.volume.set_value(if muted { 0.0 } else { percent });
            w.mute.set_icon_name(if muted {
                "audio-volume-muted-symbolic"
            } else {
                "audio-volume-high-symbolic"
            });
        } else {
            if let Some(row) = w.volume.parent() {
                row.set_visible(false);
            }
            w.panel_volume.set_visible(false);
        }
        let recording = snapshot.recording && snapshot.source.is_some();
        w.mic_row.set_visible(recording);
        w.panel_mic.set_visible(recording);
        if let Some((percent, muted)) = snapshot.source {
            let icon = logic::mic_icon(percent, muted);
            w.panel_mic.set_icon_name(Some(icon));
            if muted {
                w.panel_mic.remove_css_class("privacy-indicator");
            } else {
                w.panel_mic.add_css_class("privacy-indicator");
            }
            w.mic_mute.set_icon_name(icon);
            w.mic_mute
                .update_property(&[gtk::accessible::Property::Label(if muted {
                    "Unmute"
                } else {
                    "Mute"
                })]);
            w.mic.set_value(if muted { 0.0 } else { percent });
        }
        self.syncing.set(false);
        if snapshot.recording != self.last.borrow().recording {
            eprintln!("tuna-shell-gtk: audio recording event {recording}");
            if let Some(panel) = w.panel_mic.ancestor(gtk::Window::static_type()) {
                panel.add_tick_callback(move |_, _| {
                    eprintln!("tuna-shell-gtk: audio recording frame {recording}");
                    glib::ControlFlow::Break
                });
            }
        }
        // Keep menu actors alive on volume/recording events; rebuild only
        // when the set or selection of output routes actually changes.
        if snapshot.outputs != self.last.borrow().outputs {
            while let Some(child) = w.sound_list.first_child() {
                w.sound_list.remove(&child);
            }
            for output in &snapshot.outputs {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                row.append(&gtk::Image::from_icon_name(logic::sink_icon(&output.name)));
                let label = gtk::Label::new(Some(&output.name));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.append(&label);
                let check = gtk::Image::from_icon_name("ornament-check-symbolic");
                check.set_visible(output.selected);
                row.append(&check);
                let button = gtk::Button::builder().child(&row).build();
                button.add_css_class("qs-menu-item");
                button.update_property(&[gtk::accessible::Property::Label(&output.name)]);
                let output = output.clone();
                button.connect_clicked(move |b| {
                    if let Some(popover) = b
                        .ancestor(gtk::Popover::static_type())
                        .and_downcast::<gtk::Popover>()
                    {
                        popover.popdown();
                    }
                    let sink = output.sink.to_string();
                    if let (Some(device), Some((index, profile_device))) =
                        (output.device, output.route)
                    {
                        // Route is a device parameter, while wpctl's default
                        // is a node. Set the physical port before the default.
                        let params =
                            format!("{{ index: {index}, device: {profile_device}, save: true }}");
                        audio_command(
                            &["pw-cli", "set-param", &device.to_string(), "Route", &params],
                            move |ok| {
                                if ok {
                                    wpctl(&["set-default", &sink], |_| {});
                                }
                            },
                        );
                    } else {
                        wpctl(&["set-default", &sink], |_| {});
                    }
                });
                w.sound_list.append(&button);
            }
            w.sound_arrow.set_visible(snapshot.outputs.len() > 1);
        }
        *self.last.borrow_mut() = snapshot;
    }

    fn retry(self: &Rc<Self>) {
        if let Some(process) = self.process.borrow_mut().take() {
            process.force_exit();
        }
        *self.state.borrow_mut() = crate::audio_state::AudioState::default();
        self.update(crate::audio_state::Snapshot::default());
        if self.stopping.get() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local_once(2, move || {
            if let Some(ui) = weak.upgrade() {
                ui.start();
            }
        });
    }

    fn start(self: &Rc<Self>) {
        if self.stopping.get() {
            return;
        }
        let launcher = gio::SubprocessLauncher::new(
            gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_SILENCE,
        );
        let parent = std::process::id() as libc::pid_t;
        // This callback runs between fork and exec. It uses only Linux
        // syscalls, without allocating or touching GLib/thread locks.
        launcher.set_child_setup(move || unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) != 0
                || libc::getppid() != parent
            {
                libc::_exit(127);
            }
        });
        match launcher.spawn(&[
            "pw-dump".as_ref(),
            "--monitor".as_ref(),
            "--no-colors".as_ref(),
        ]) {
            Ok(process) => {
                let Some(stream) = process.stdout_pipe() else {
                    self.retry();
                    return;
                };
                *self.process.borrow_mut() = Some(process);
                self.read(stream);
            }
            Err(_) => self.retry(),
        }
    }

    fn read(self: &Rc<Self>, stream: gio::InputStream) {
        let ui = self.clone();
        let next = stream.clone();
        stream.read_bytes_async(
            65536,
            glib::Priority::DEFAULT,
            None::<&gio::Cancellable>,
            move |result| {
                if ui.stopping.get() {
                    return;
                }
                let Ok(bytes) = result else {
                    ui.retry();
                    return;
                };
                if bytes.is_empty() {
                    ui.retry();
                    return;
                }
                let changed = ui.state.borrow_mut().feed(bytes.as_ref());
                match changed {
                    Ok(true) => {
                        let snapshot = ui.state.borrow().snapshot();
                        ui.update(snapshot);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("tuna-shell-gtk: {e}");
                        ui.retry();
                        return;
                    }
                }
                ui.read(next);
            },
        );
    }
}

fn audio_command(args: &[&str], done: impl FnOnce(bool) + 'static) {
    let argv: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
    let Ok(process) = gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
    ) else {
        done(false);
        return;
    };
    process
        .clone()
        .wait_check_async(None::<&gio::Cancellable>, move |result| {
            done(result.is_ok())
        });
    let weak = process.downgrade();
    glib::timeout_add_seconds_local_once(2, move || {
        if let Some(process) = weak.upgrade() {
            process.force_exit();
        }
    });
}

fn audio(w: &Rc<Widgets>) {
    // Hide initial default actors until the first complete graph arrives.
    if let Some(row) = w.volume.parent() {
        row.set_visible(false);
    }
    w.panel_volume.set_visible(false);
    w.mic_row.set_visible(false);
    w.panel_mic.set_visible(false);
    w.sound_arrow.set_visible(false);
    let ui = Rc::new(AudioUi {
        widgets: w.clone(),
        state: RefCell::default(),
        last: RefCell::default(),
        syncing: Cell::new(false),
        process: RefCell::new(None),
        stopping: Cell::new(false),
        sound: crate::media_keys::sound_settings(),
    });
    ui.refresh_volume_limit();
    if let Some(sound) = ui.sound.as_ref() {
        let weak = Rc::downgrade(&ui);
        sound.connect_changed(Some("allow-volume-above-100-percent"), move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.refresh_volume_limit();
            }
        });
    }
    for (slider, node) in [(&w.volume, SINK), (&w.mic, SOURCE)] {
        let ui = ui.clone();
        slider.connect_value_changed(move |s| {
            if !ui.syncing.get() {
                wpctl(
                    &[
                        "set-volume",
                        node,
                        &if node == SINK {
                            logic::wpctl_volume_arg_with_limit(
                                s.value(),
                                crate::media_keys::output_limit(ui.sound.as_ref()),
                            )
                        } else {
                            logic::wpctl_volume_arg(s.value())
                        },
                    ],
                    |_| {},
                );
            }
        });
    }
    w.mute
        .connect_clicked(|_| wpctl(&["set-mute", SINK, "toggle"], |_| {}));
    {
        let ui = ui.clone();
        w.mic_mute.connect_clicked(move |_| {
            if ui.last.borrow().source.is_some_and(|(v, m)| m && v <= 0.0) {
                wpctl(&["set-volume", SOURCE, "0.25"], |_| {
                    wpctl(&["set-mute", SOURCE, "toggle"], |_| {})
                });
            } else {
                wpctl(&["set-mute", SOURCE, "toggle"], |_| {});
            }
        });
    }
    if let Some(app) = gio::Application::default() {
        let weak = Rc::downgrade(&ui);
        app.connect_shutdown(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.stopping.set(true);
                if let Some(p) = ui.process.borrow_mut().take() {
                    p.force_exit();
                }
            }
        });
    }
    ui.start();
}

#[cfg(test)]
mod monitor_backlight_tests {
    use super::*;
    #[test]
    fn monitor_brightness_never_uses_a_different_connectors_backlight() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("backlight");
        std::fs::create_dir_all(&root).unwrap();
        for (name, connector) in [("aaa-external", "HDMI-A-1"), ("intel-backlight", "eDP-1")] {
            let path = root.join(name);
            let device = temp.path().join(format!("drm/card0-{connector}"));
            std::fs::create_dir_all(&path).unwrap();
            std::fs::create_dir_all(&device).unwrap();
            symlink(device, path.join("device")).unwrap();
        }
        assert_eq!(
            backlight_for(&root, Some("eDP-1")).unwrap().0,
            "intel-backlight"
        );
        assert_eq!(
            backlight_for(&root, Some("HDMI-A-1")).unwrap().0,
            "aaa-external"
        );
        assert!(backlight_for(&root, Some("DP-2")).is_none());
    }
}

/// UPower's aggregate display device describes all laptop batteries together.
fn battery(conn: &gio::DBusConnection, w: &Rc<Widgets>) {
    const NAME: &str = "org.freedesktop.UPower";
    let remote = Remote::new(
        conn,
        NAME,
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
    );
    let settings = crate::settings(crate::INTERFACE_SCHEMA).filter(|settings| {
        settings
            .settings_schema()
            .is_some_and(|schema| schema.has_key("show-battery-percentage"))
    });
    let refresh: Rc<dyn Fn()> = {
        let (remote, w, settings) = (remote.clone(), w.clone(), settings.clone());
        Rc::new(move || {
            let (w, settings) = (w.clone(), settings.clone());
            remote.get_all(move |props| {
                let present = props.as_ref().is_some_and(|p| {
                    dict_bool(p, "IsPresent").unwrap_or(false)
                        && p.lookup_value("Type", None).and_then(|v| v.get::<u32>()) == Some(2)
                });
                w.panel_battery.set_visible(present);
                w.battery_summary.set_visible(present);
                let Some(props) = props.filter(|_| present) else {
                    return;
                };
                let percent = props
                    .lookup_value("Percentage", None)
                    .and_then(|v| v.get::<f64>())
                    .unwrap_or(0.0)
                    .clamp(0.0, 100.0)
                    .round() as u32;
                let state = props
                    .lookup_value("State", None)
                    .and_then(|v| v.get::<u32>())
                    .unwrap_or(0);
                w.battery_icon
                    .set_icon_name(Some(&battery_icon(percent, state)));
                w.battery_percentage.set_text(&format!("{percent}%"));
                w.battery_percentage.set_visible(
                    settings
                        .as_ref()
                        .is_some_and(|s| s.boolean("show-battery-percentage")),
                );
                let seconds = props
                    .lookup_value(
                        if state == 1 {
                            "TimeToFull"
                        } else {
                            "TimeToEmpty"
                        },
                        None,
                    )
                    .and_then(|v| v.get::<i64>())
                    .unwrap_or(0);
                let summary = battery_summary(percent, state, seconds);
                w.battery_summary.set_text(&summary);
                w.panel_battery.set_tooltip_text(Some(&summary));
                w.battery_icon
                    .update_property(&[gtk::accessible::Property::Label(&summary)]);
            });
        })
    };
    {
        let refresh = refresh.clone();
        remote.watch(move || refresh());
    }
    if let Some(settings) = settings {
        let refresh = refresh.clone();
        settings.connect_changed(Some("show-battery-percentage"), move |_, _| refresh());
    }
    watch_name(conn, NAME, move |_| refresh());
}

fn battery_icon(percent: u32, state: u32) -> String {
    if state == 3 {
        return "battery-empty-symbolic".into();
    }
    if state == 4 {
        return "battery-level-100-charged-symbolic".into();
    }
    let level = percent.min(100) / 10 * 10;
    format!(
        "battery-level-{level}{}-symbolic",
        if matches!(state, 1 | 5) {
            "-charging"
        } else {
            ""
        }
    )
}

fn battery_summary(percent: u32, state: u32, seconds: i64) -> String {
    let status = match state {
        1 | 5 => "Charging",
        3 => "Empty",
        4 => "Fully charged",
        _ => "Discharging",
    };
    if seconds > 0 && matches!(state, 1 | 2) {
        let minutes = seconds / 60;
        format!(
            "{percent}% · {status} · {}∶{:02} {}",
            minutes / 60,
            minutes % 60,
            if state == 1 {
                "until full"
            } else {
                "remaining"
            }
        )
    } else {
        format!("{percent}% · {status}")
    }
}

#[cfg(test)]
mod battery_tests {
    use super::*;
    #[test]
    fn upower_battery_states_choose_icons_and_remaining_time() {
        assert_eq!(battery_icon(37, 2), "battery-level-30-symbolic");
        assert_eq!(battery_icon(37, 1), "battery-level-30-charging-symbolic");
        assert_eq!(battery_icon(0, 3), "battery-empty-symbolic");
        assert_eq!(battery_icon(100, 4), "battery-level-100-charged-symbolic");
        assert_eq!(
            battery_summary(37, 2, 5400),
            "37% · Discharging · 1∶30 remaining"
        );
        assert_eq!(
            battery_summary(82, 1, 1800),
            "82% · Charging · 0∶30 until full"
        );
    }
}
