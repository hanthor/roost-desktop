//! `org.gnome.Shell` on the session bus: the part of GNOME Shell's own
//! D-Bus interface that other GNOME components call (shellDBus.js).
//! gnome-settings-daemon's media keys show the volume and brightness OSD
//! through `ShowOSD`, and open search or the app grid through
//! `FocusSearch` and `ShowApplications`.
//!
//! As in GNOME, those methods answer only allowlisted callers (Settings,
//! the media-keys daemon, the GNOME portal backend). Harnesses set
//! `ROOST_SHELL_DBUS_UNRESTRICTED=1`, GNOME's unsafe mode.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use crate::osd::OsdRequest;
use glib::translate::IntoGlib;

pub const NAME: &str = "org.gnome.Shell";
pub const PATH: &str = "/org/gnome/Shell";

/// GNOME's DBusSenderChecker allowlist for these methods.
pub const ALLOWED_SENDERS: &[&str] = &[
    "org.gnome.Settings",
    "org.gnome.SettingsDaemon.MediaKeys",
    "org.freedesktop.impl.portal.desktop.gnome",
];

const XML: &str = r#"<node>
  <interface name="org.gnome.Shell">
    <method name="ShowOSD"><arg type="a{sv}" direction="in" name="params"/></method>
    <method name="FocusSearch"/>
    <method name="ShowApplications"/>
    <method name="GrabAccelerator">
      <arg type="s" direction="in" name="accelerator"/>
      <arg type="u" direction="in" name="modeFlags"/>
      <arg type="u" direction="in" name="grabFlags"/>
      <arg type="u" direction="out" name="action"/>
    </method>
    <method name="GrabAccelerators">
      <arg type="a(suu)" direction="in" name="accelerators"/>
      <arg type="au" direction="out" name="actions"/>
    </method>
    <method name="UngrabAccelerator">
      <arg type="u" direction="in" name="action"/>
      <arg type="b" direction="out" name="success"/>
    </method>
    <method name="UngrabAccelerators">
      <arg type="au" direction="in" name="action"/>
      <arg type="b" direction="out" name="success"/>
    </method>
    <signal name="AcceleratorActivated">
      <arg name="action" type="u"/>
      <arg name="parameters" type="a{sv}"/>
    </signal>
    <signal name="AcceleratorDeactivated">
      <arg name="action" type="u"/><arg name="parameters" type="a{sv}"/>
    </signal>
    <property name="Mode" type="s" access="read"/>
    <property name="OverviewActive" type="b" access="read"/>
    <property name="ShellVersion" type="s" access="read"/>
  </interface>
</node>"#;

/// What the methods act on.
pub trait ShellActions {
    fn show_osd(&self, request: &OsdRequest);
    fn focus_search(&self);
    fn show_applications(&self);
    fn overview_active(&self) -> bool;
    /// Hand the compositor the full set of grabs.
    fn set_accelerator_grabs(&self, grabs: Vec<roost_shell_control::AcceleratorGrab>);
}

/// Parse a GNOME accelerator string (`<Super>p`, `XF86AudioRaiseVolume`)
/// into the compositor's keysym and modifier bits; `None` when it does
/// not parse or names no key.
pub fn parse_accelerator(accelerator: &str) -> Option<(u32, u32)> {
    // Mutter's syntax: `<Modifier>` tokens, then a key name.
    let mut rest = accelerator.trim();
    let mut bits = 0;
    while let Some(after) = rest.strip_prefix('<') {
        let (token, tail) = after.split_once('>')?;
        bits |= match token.to_ascii_lowercase().as_str() {
            "shift" => roost_shell_control::MOD_SHIFT,
            "control" | "ctrl" | "primary" => roost_shell_control::MOD_CTRL,
            "alt" | "mod1" | "meta" => roost_shell_control::MOD_ALT,
            "super" | "mod4" | "hyper" => roost_shell_control::MOD_LOGO,
            _ => return None,
        };
        rest = tail;
    }
    if rest.is_empty() {
        return None;
    }
    // Letters grab as their lower case, as GTK and Mutter parse them.
    // Names GDK lacks (XF86Keyboard) come from xkbcommon, as Mutter's.
    let key = match gtk4::gdk::Key::from_name(rest) {
        Some(key) => key.to_lower().into_glib(),
        None => {
            let sym = xkbcommon::xkb::keysym_from_name(rest, xkbcommon::xkb::KEYSYM_NO_FLAGS);
            if sym.raw() == 0 {
                return None;
            }
            sym.raw()
        }
    };
    Some((key, bits))
}

/// One caller's grab.
#[derive(Debug, Clone)]
struct Grab {
    sender: String,
    keysym: u32,
    mods: u32,
    modes: u32,
    flags: u32,
}

/// Every grab by action id (Mutter's keybinding actions).
#[derive(Debug, Default)]
struct Grabs {
    next: u32,
    by_action: BTreeMap<u32, Grab>,
    /// Keys the shell binds itself (its own keybindings): not grabbable.
    reserved: Vec<(u32, u32)>,
}

impl Grabs {
    /// Grab `accelerator` for `sender`: the new action id, or 0 when it
    /// does not parse or is grabbed already (as Mutter answers).
    #[cfg(test)]
    fn grab(&mut self, sender: &str, accelerator: &str, modes: u32) -> u32 {
        self.grab_with_flags(sender, accelerator, modes, 0)
    }

    fn grab_with_flags(&mut self, sender: &str, accelerator: &str, modes: u32, flags: u32) -> u32 {
        if flags & !roost_shell_control::SUPPORTED_GRAB_FLAGS != 0 {
            return 0;
        }
        let Some((keysym, mods)) = parse_accelerator(accelerator) else {
            return 0;
        };
        if self.reserved.contains(&(keysym, mods))
            || self
                .by_action
                .values()
                .any(|g| g.keysym == keysym && g.mods == mods)
            || self.by_action.len() >= roost_shell_control::MAX_ACCELERATORS
        {
            return 0;
        }
        let Some(next) = self
            .next
            .checked_add(1)
            .filter(|next| *next < INTERNAL_BASE)
        else {
            return 0;
        };
        self.next = next;
        // GNOME's modes; none given means the normal session.
        let modes = if modes == 0 {
            roost_shell_control::MODE_NORMAL
        } else {
            modes
        };
        self.by_action.insert(
            self.next,
            Grab {
                sender: sender.to_owned(),
                keysym,
                mods,
                modes,
                flags,
            },
        );
        self.next
    }

    /// Release `sender`'s grab of `action`.
    fn ungrab(&mut self, sender: &str, action: u32) -> bool {
        match self.by_action.get(&action) {
            Some(g) if g.sender == sender => {
                self.by_action.remove(&action);
                true
            }
            _ => false,
        }
    }

    fn forget(&mut self, sender: &str) -> bool {
        let before = self.by_action.len();
        self.by_action.retain(|_, g| g.sender != sender);
        before != self.by_action.len()
    }

    fn list(&self) -> Vec<roost_shell_control::Accelerator> {
        self.by_action
            .iter()
            .map(|(action, g)| roost_shell_control::Accelerator {
                action: *action,
                keysym: g.keysym,
                mods: g.mods,
                modes: g.modes,
            })
            .collect()
    }
}

/// Runs one of the shell's own keybindings by index.
type InternalRun = Rc<dyn Fn(usize)>;

/// Action ids of the shell's own keybindings start here, clear of the
/// ids D-Bus callers get.
pub const INTERNAL_BASE: u32 = 0x4000_0000;

/// The running service: the shell reports accelerator presses here.
pub struct Service {
    conn: RefCell<Option<gio::DBusConnection>>,
    grabs: RefCell<Grabs>,
    /// The shell's own keybindings, ids from [`INTERNAL_BASE`].
    internal: RefCell<Vec<roost_shell_control::Accelerator>>,
    /// Runs an internal binding by its index.
    on_internal: RefCell<Option<InternalRun>>,
    actions: Rc<dyn ShellActions>,
    /// NameOwnerChanged, to drop a departed caller's grabs.
    watch: RefCell<Option<gio::SignalSubscription>>,
}

impl Service {
    /// Hand the compositor every grab: callers' and the shell's own.
    fn push(&self) {
        let mut list = self.grabs.borrow().list();
        let grabs = self.grabs.borrow();
        let mut typed: Vec<_> = list
            .drain(..)
            .map(|accelerator| roost_shell_control::AcceleratorGrab {
                flags: grabs.by_action[&accelerator.action].flags,
                accelerator,
            })
            .collect();
        typed.extend(self.internal.borrow().iter().copied().map(|accelerator| {
            roost_shell_control::AcceleratorGrab {
                accelerator,
                flags: 0,
            }
        }));
        self.actions.set_accelerator_grabs(typed);
    }

    /// Bind the shell's own keys: `accelerators[i]` with its modes runs
    /// `run(i)`. Keys that do not parse are skipped.
    pub fn set_internal(&self, accelerators: &[(String, u32)], run: InternalRun) {
        let mut list = Vec::new();
        let mut reserved = Vec::new();
        for (i, (accel, modes)) in accelerators.iter().enumerate() {
            if let Some((keysym, mods)) = parse_accelerator(accel) {
                reserved.push((keysym, mods));
                list.push(roost_shell_control::Accelerator {
                    action: INTERNAL_BASE + i as u32,
                    keysym,
                    mods,
                    modes: *modes,
                });
            }
        }
        self.grabs.borrow_mut().reserved = reserved;
        *self.internal.borrow_mut() = list;
        *self.on_internal.borrow_mut() = Some(run);
        self.push();
    }

    /// A grabbed accelerator fired: one of the shell's own runs here;
    /// a caller's is signalled to that caller, as GNOME does (unicast
    /// AcceleratorActivated).
    pub fn accelerator_activated(&self, action: u32, time: u32, mode: u32) {
        self.accelerator_event(action, time, mode, false);
    }

    pub fn accelerator_deactivated(&self, action: u32, time: u32, mode: u32) {
        self.accelerator_event(action, time, mode, true);
    }

    fn accelerator_event(&self, action: u32, time: u32, mode: u32, released: bool) {
        if action >= INTERNAL_BASE {
            if released {
                return;
            }
            let run = self.on_internal.borrow().clone();
            if let Some(run) = run {
                run((action - INTERNAL_BASE) as usize);
            }
            return;
        }
        let Some(sender) = self
            .grabs
            .borrow()
            .by_action
            .get(&action)
            .map(|g| g.sender.clone())
        else {
            return;
        };
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let params = glib::VariantDict::new(None);
        params.insert("timestamp", time);
        params.insert("action-mode", mode);
        params.insert("device-node", "");
        params.insert("device-id", 0u32);
        let _ = conn.emit_signal(
            Some(&sender),
            PATH,
            "org.gnome.Shell",
            if released {
                "AcceleratorDeactivated"
            } else {
                "AcceleratorActivated"
            },
            Some(&(action, params.end()).to_variant()),
        );
    }
}

/// Unpack ShowOSD's `a{sv}` the way shellDBus.js does.
pub fn parse_osd(params: &glib::Variant) -> OsdRequest {
    let dict = glib::VariantDict::new(Some(params));
    OsdRequest {
        icon: dict.lookup::<String>("icon").ok().flatten(),
        label: dict.lookup::<String>("label").ok().flatten(),
        level: dict.lookup::<f64>("level").ok().flatten(),
        max_level: dict.lookup::<f64>("max_level").ok().flatten(),
    }
}

/// Whether `sender` (a unique bus name) owns one of the allowlisted names.
fn sender_allowed(conn: &gio::DBusConnection, sender: Option<&str>) -> bool {
    if std::env::var_os("ROOST_SHELL_DBUS_UNRESTRICTED").is_some_and(|v| v == "1") {
        return true;
    }
    let Some(sender) = sender else {
        return false;
    };
    ALLOWED_SENDERS.iter().any(|name| {
        conn.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            Some(&(*name,).to_variant()),
            Some(glib::VariantTy::new("(s)").unwrap()),
            gio::DBusCallFlags::NONE,
            500,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|v| v.get::<(String,)>())
        .is_some_and(|(owner,)| owner == sender)
    })
}

/// Own `org.gnome.Shell` and serve it for the session. Another owner
/// (a real GNOME Shell) keeps the name.
pub fn start(actions: Rc<dyn ShellActions>) -> Rc<Service> {
    let service = Rc::new(Service {
        conn: RefCell::new(None),
        grabs: RefCell::new(Grabs::default()),
        internal: RefCell::new(Vec::new()),
        on_internal: RefCell::new(None),
        actions: actions.clone(),
        watch: RefCell::new(None),
    });
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("roost-shell-gtk: org.gnome.Shell interface: {e}");
            return service;
        }
    };
    let Some(info) = node.lookup_interface("org.gnome.Shell") else {
        return service;
    };
    let svc = service.clone();
    let id = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            *svc.conn.borrow_mut() = Some(conn.clone());
            let calls = actions.clone();
            let props = actions.clone();
            let grabs_svc = svc.clone();
            let registered = conn
                .register_object(PATH, &info)
                .method_call(move |conn, sender, _, _, method, params, invocation| {
                    if !sender_allowed(&conn, sender) {
                        invocation.return_dbus_error(
                            "org.freedesktop.DBus.Error.AccessDenied",
                            &format!("{method} is not allowed"),
                        );
                        return;
                    }
                    let sender = sender.unwrap_or_default().to_owned();
                    let reply = match method {
                        "ShowOSD" => {
                            let request = params
                                .try_child_value(0)
                                .map(|p| parse_osd(&p))
                                .unwrap_or_default();
                            calls.show_osd(&request);
                            None
                        }
                        "FocusSearch" => {
                            calls.focus_search();
                            None
                        }
                        "ShowApplications" => {
                            calls.show_applications();
                            None
                        }
                        "GrabAccelerator" => {
                            let Some((accel, modes, flags)) = params.get::<(String, u32, u32)>()
                            else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    method,
                                );
                                return;
                            };
                            let action = grabs_svc
                                .grabs
                                .borrow_mut()
                                .grab_with_flags(&sender, &accel, modes, flags);
                            grabs_svc.push();
                            Some((action,).to_variant())
                        }
                        "GrabAccelerators" => {
                            let Some((list,)) = params.get::<(Vec<(String, u32, u32)>,)>() else {
                                invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.InvalidArgs",
                                    method,
                                );
                                return;
                            };
                            let actions: Vec<u32> = {
                                let mut grabs = grabs_svc.grabs.borrow_mut();
                                list.iter()
                                    .map(|(accel, modes, flags)| {
                                        grabs.grab_with_flags(&sender, accel, *modes, *flags)
                                    })
                                    .collect()
                            };
                            grabs_svc.push();
                            Some((actions,).to_variant())
                        }
                        "UngrabAccelerator" => {
                            let action = params.get::<(u32,)>().map(|(a,)| a).unwrap_or(0);
                            let ok = grabs_svc.grabs.borrow_mut().ungrab(&sender, action);
                            grabs_svc.push();
                            Some((ok,).to_variant())
                        }
                        "UngrabAccelerators" => {
                            let list = params
                                .get::<(Vec<u32>,)>()
                                .map(|(l,)| l)
                                .unwrap_or_default();
                            let ok = {
                                let mut grabs = grabs_svc.grabs.borrow_mut();
                                {
                                    // Every action is released even after a miss.
                                    let mut ok = true;
                                    for action in &list {
                                        ok &= grabs.ungrab(&sender, *action);
                                    }
                                    ok
                                }
                            };
                            grabs_svc.push();
                            Some((ok,).to_variant())
                        }
                        _ => {
                            invocation.return_dbus_error(
                                "org.freedesktop.DBus.Error.UnknownMethod",
                                method,
                            );
                            return;
                        }
                    };
                    invocation.return_value(reply.as_ref());
                })
                .property(move |_, _, _, _, property| match property {
                    "Mode" => "user".to_variant(),
                    "OverviewActive" => props.overview_active().to_variant(),
                    "ShellVersion" => "51.0".to_variant(),
                    _ => ().to_variant(),
                })
                .build();
            if let Err(e) = registered {
                eprintln!("roost-shell-gtk: org.gnome.Shell object: {e}");
            }
            // A caller that leaves the bus loses its grabs, as in GNOME.
            let leave_svc = svc.clone();
            let weak = Rc::downgrade(&leave_svc);
            let subscription = conn.subscribe_to_signal(
                Some("org.freedesktop.DBus"),
                Some("org.freedesktop.DBus"),
                Some("NameOwnerChanged"),
                Some("/org/freedesktop/DBus"),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let Some(svc) = weak.upgrade() else { return };
                    if let Some((name, _, new_owner)) =
                        signal.parameters.get::<(String, String, String)>()
                    {
                        if new_owner.is_empty() && svc.grabs.borrow_mut().forget(&name) {
                            svc.push();
                        }
                    }
                },
            );
            *leave_svc.watch.borrow_mut() = Some(subscription);
        },
        |_, _| {},
        |_, _| eprintln!("roost-shell-gtk: org.gnome.Shell is owned elsewhere"),
    );
    // The name is held for the session: an OwnerId releases nothing
    // when it drops.
    let _ = id;
    service
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_osd_params_unpack_like_gnome() {
        let dict = glib::VariantDict::new(None);
        dict.insert("icon", "audio-volume-medium-symbolic");
        dict.insert("level", 0.5f64);
        dict.insert("max_level", 1.5f64);
        let request = parse_osd(&dict.end());
        assert_eq!(
            request,
            OsdRequest {
                icon: Some("audio-volume-medium-symbolic".into()),
                label: None,
                level: Some(0.5),
                max_level: Some(1.5),
            }
        );
        let dict = glib::VariantDict::new(None);
        dict.insert("label", "English (US)");
        let request = parse_osd(&dict.end());
        assert_eq!(request.label.as_deref(), Some("English (US)"));
        assert_eq!(request.icon, None);
    }

    #[test]
    fn accelerators_parse_like_gnome() {
        let (vol, mods) = parse_accelerator("XF86AudioRaiseVolume").unwrap();
        assert_eq!((vol, mods), (0x1008FF13, 0));
        let (p, mods) = parse_accelerator("<Super>p").unwrap();
        assert_eq!(p, u32::from(b'p'));
        assert_eq!(mods, roost_shell_control::MOD_LOGO);
        let (_, mods) = parse_accelerator("<Primary><Alt>Delete").unwrap();
        assert_eq!(
            mods,
            roost_shell_control::MOD_CTRL | roost_shell_control::MOD_ALT
        );
        assert!(parse_accelerator("not a key").is_none());
    }

    #[test]
    fn grabs_belong_to_their_caller() {
        let mut grabs = Grabs::default();
        let a = grabs.grab(":1.5", "XF86AudioMute", 0);
        assert!(a > 0);
        // The same combination cannot be grabbed twice.
        assert_eq!(grabs.grab(":1.6", "XF86AudioMute", 0), 0);
        assert_eq!(grabs.grab(":1.6", "garbage", 0), 0);
        assert!(!grabs.ungrab(":1.6", a), "only the owner ungrabs");
        let list = grabs.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].modes, roost_shell_control::MODE_NORMAL);
        assert!(grabs.forget(":1.5"));
        assert!(grabs.list().is_empty());
    }
}

#[cfg(test)]
mod grab_flag_tests {
    use super::*;
    #[test]
    fn unsupported_flags_return_zero_without_installing_and_reply_indexes_remain_stable() {
        let mut grabs = Grabs::default();
        let requests = [("r", 128), ("s", 8), ("t", 16), ("r", 128), ("u", 0)];
        let actions: Vec<_> = requests
            .iter()
            .map(|(key, flags)| grabs.grab_with_flags(":1.70", key, 1, *flags))
            .collect();
        assert_eq!(actions, [1, 0, 2, 0, 3]);
        assert_eq!(grabs.by_action.len(), 3);
        assert!(!grabs
            .by_action
            .values()
            .any(|grab| grab.keysym == u32::from(b's')));
        assert_eq!(grabs.by_action[&1].flags, 128);
        for flags in [1, 2, 4, 8, 32, 64, 256, u32::MAX] {
            assert_eq!(grabs.grab_with_flags(":1.71", "v", 1, flags), 0);
        }
    }
    #[test]
    fn exhausted_action_ids_do_not_recycle_into_old_or_internal_owners() {
        let mut grabs = Grabs {
            next: INTERNAL_BASE - 1,
            ..Default::default()
        };
        assert_eq!(grabs.grab_with_flags(":1.72", "r", 1, 128), 0);
        assert!(grabs.by_action.is_empty());
    }
}
