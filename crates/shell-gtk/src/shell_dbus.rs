//! `org.gnome.Shell` on the session bus: the part of GNOME Shell's own
//! D-Bus interface that other GNOME components call (shellDBus.js).
//! gnome-settings-daemon's media keys show the volume and brightness OSD
//! through `ShowOSD`, and open search or the app grid through
//! `FocusSearch` and `ShowApplications`.
//!
//! As in GNOME, those methods answer only allowlisted callers (Settings,
//! the media-keys daemon, the GNOME portal backend). Harnesses set
//! `ROOST_SHELL_DBUS_UNRESTRICTED=1`, GNOME's unsafe mode.

use std::rc::Rc;

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;

use crate::osd::OsdRequest;

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
pub fn start(actions: Rc<dyn ShellActions>) {
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("roost-shell-gtk: org.gnome.Shell interface: {e}");
            return;
        }
    };
    let Some(info) = node.lookup_interface("org.gnome.Shell") else {
        return;
    };
    let id = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::NONE,
        move |conn, _| {
            let calls = actions.clone();
            let props = actions.clone();
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
                    match method {
                        "ShowOSD" => {
                            let request = params
                                .try_child_value(0)
                                .map(|p| parse_osd(&p))
                                .unwrap_or_default();
                            calls.show_osd(&request);
                        }
                        "FocusSearch" => calls.focus_search(),
                        "ShowApplications" => calls.show_applications(),
                        _ => {
                            invocation.return_dbus_error(
                                "org.freedesktop.DBus.Error.UnknownMethod",
                                method,
                            );
                            return;
                        }
                    }
                    invocation.return_value(None);
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
        },
        |_, _| {},
        |_, _| eprintln!("roost-shell-gtk: org.gnome.Shell is owned elsewhere"),
    );
    // The name is held for the session: an OwnerId releases nothing
    // when it drops.
    let _ = id;
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
}
