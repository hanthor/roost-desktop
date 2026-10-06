//! GNOME Shell's `org.gnome.Shell.Introspect` (#61): the window list
//! xdg-desktop-portal-gnome shows in its "Share a window" picker, and
//! the running-apps list it uses for app-scoped sharing. Adapted from
//! niri's `src/dbus/gnome_shell_introspect.rs` (GPL-3.0-or-later, like
//! Roost), with GNOME Shell's interface shape (`shell-introspect.c`):
//! version 3, `WindowsChanged` on every change, and the same caller
//! policy, since window titles are private. Only the desktop portal
//! may ask, unless `ROOST_INTROSPECT_UNRESTRICTED=1` (GNOME's "unsafe
//! mode", for tests and development).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use zbus::message::Header;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{fdo, interface, Connection};

use crate::mutter::{WindowSnapshot, Windows};

pub const NAME: &str = "org.gnome.Shell.Introspect";
pub const PATH: &str = "/org/gnome/Shell/Introspect";
pub const UNRESTRICTED_ENV: &str = "ROOST_INTROSPECT_UNRESTRICTED";

struct Service {
    authority: crate::capture_security::Authority,
    windows: Windows,
    screen_size: Arc<Mutex<(i32, i32)>>,
    unrestricted: bool,
    animations_enabled: Arc<AtomicBool>,
}

/// GNOME's app id for a window: its desktop file id, or `window:N` for
/// a window no app claims.
pub fn app_id(window: &WindowSnapshot) -> String {
    match window.app_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) if id.ends_with(".desktop") => id.to_owned(),
        Some(id) => format!("{id}.desktop"),
        None => format!("window:{}", window.id),
    }
}

/// One window's `a{sv}`, keyed like GNOME Shell's.
pub fn window_properties(window: &WindowSnapshot) -> HashMap<String, OwnedValue> {
    let mut props = HashMap::new();
    let mut put = |key: &str, value: Value<'_>| {
        if let Ok(value) = OwnedValue::try_from(value) {
            props.insert(key.to_owned(), value);
        }
    };
    put("title", Value::from(window.title.clone()));
    put("app-id", Value::from(app_id(window)));
    put("client-type", Value::from(u32::from(window.x11)));
    put("is-hidden", Value::from(window.hidden));
    put("has-focus", Value::from(window.focused));
    put("width", Value::from(window.width.max(0) as u32));
    put("height", Value::from(window.height.max(0) as u32));
    if let Some(class) = window.app_id.as_ref().filter(|_| window.x11) {
        put("wm-class", Value::from(class.clone()));
    }
    props
}

/// Running apps keyed by app id, each with the seats it has focus on.
pub fn running_apps(windows: &[WindowSnapshot]) -> HashMap<String, HashMap<String, OwnedValue>> {
    let mut apps: HashMap<String, bool> = HashMap::new();
    for window in windows {
        *apps.entry(app_id(window)).or_default() |= window.focused;
    }
    apps.into_iter()
        .map(|(id, focused)| {
            let seats: Vec<String> = if focused {
                vec!["seat0".to_owned()]
            } else {
                Vec::new()
            };
            let mut props = HashMap::new();
            if let Ok(value) = OwnedValue::try_from(Value::from(seats)) {
                props.insert("active-on-seats".to_owned(), value);
            }
            (id, props)
        })
        .collect()
}

impl Service {
    async fn check_caller(&self, conn: &Connection, header: &Header<'_>) -> fdo::Result<()> {
        self.authority.unlocked()?;
        if self.unrestricted {
            return Ok(());
        }
        self.authority.admit(conn, header).await.map(|_| ())
    }

    fn snapshot(&self) -> Vec<WindowSnapshot> {
        self.windows.lock().map(|w| w.clone()).unwrap_or_default()
    }
}

#[interface(name = "org.gnome.Shell.Introspect")]
impl Service {
    async fn get_windows(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<HashMap<u64, HashMap<String, OwnedValue>>> {
        self.check_caller(conn, &header).await?;
        Ok(self
            .snapshot()
            .iter()
            .map(|w| (w.id, window_properties(w)))
            .collect())
    }

    async fn get_running_applications(
        &self,
        #[zbus(connection)] conn: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> fdo::Result<HashMap<String, HashMap<String, OwnedValue>>> {
        self.check_caller(conn, &header).await?;
        Ok(running_apps(&self.snapshot()))
    }

    #[zbus(signal)]
    async fn windows_changed(ctxt: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn running_applications_changed(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn animations_enabled(&self) -> bool {
        self.animations_enabled.load(Ordering::Acquire)
    }

    /// Logical dimensions of the whole desktop, including non-primary outputs.
    #[zbus(property)]
    fn screen_size(&self) -> (i32, i32) {
        self.screen_size.lock().map(|s| *s).unwrap_or((0, 0))
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
}

/// The event loop's side: publish the window list, signal changes.
#[derive(Clone)]
pub struct Handle {
    pub windows: Windows,
    animations_enabled: Arc<AtomicBool>,
    screen_size: Arc<Mutex<(i32, i32)>>,
    conn: Arc<OnceLock<zbus::blocking::Connection>>,
}

impl Default for Handle {
    fn default() -> Self {
        Self {
            windows: Windows::default(),
            screen_size: Arc::default(),
            animations_enabled: Arc::new(AtomicBool::new(true)),
            conn: Arc::default(),
        }
    }
}

impl Handle {
    /// Publish the same effective motion preference used by the compositor.
    pub fn publish_animations_enabled(&self, enabled: bool) {
        if self.animations_enabled.swap(enabled, Ordering::AcqRel) == enabled {
            return;
        }
        if let Some(conn) = self.conn.get() {
            let changed = HashMap::from([("AnimationsEnabled", Value::from(enabled))]);
            let _ = conn.emit_signal(
                None::<()>,
                PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(NAME, changed, Vec::<String>::new()),
            );
        }
    }

    /// Update GNOME clients only when the effective desktop dimensions change.
    pub fn publish_screen_size(&self, size: (i32, i32)) {
        let Ok(mut current) = self.screen_size.lock() else {
            return;
        };
        if *current == size {
            return;
        }
        *current = size;
        drop(current);
        if let Some(conn) = self.conn.get() {
            let changed = HashMap::from([("ScreenSize", Value::from(size))]);
            let _ = conn.emit_signal(
                None::<()>,
                PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(NAME, changed, Vec::<String>::new()),
            );
        }
    }

    /// Replace the window list; signal clients when it changed.
    pub fn publish(&self, snapshot: Vec<WindowSnapshot>) {
        let Ok(mut current) = self.windows.lock() else {
            return;
        };
        if *current == snapshot {
            return;
        }
        let apps_changed = running_apps(&current) != running_apps(&snapshot);
        *current = snapshot;
        drop(current);
        if let Some(conn) = self.conn.get() {
            let _ = conn.emit_signal(None::<()>, PATH, NAME, "WindowsChanged", &());
            if apps_changed {
                let _ = conn.emit_signal(None::<()>, PATH, NAME, "RunningApplicationsChanged", &());
            }
        }
    }
}

/// Serve Introspect on the session bus from a thread; quietly off when
/// there is no bus or a real GNOME Shell owns the name.
pub fn start(screen_size: (i32, i32), authority: crate::capture_security::Authority) -> Handle {
    let handle = Handle::default();
    handle.publish_screen_size(screen_size);
    let service = Service {
        authority,
        windows: handle.windows.clone(),
        screen_size: handle.screen_size.clone(),
        animations_enabled: handle.animations_enabled.clone(),
        unrestricted: std::env::var(UNRESTRICTED_ENV).is_ok_and(|v| v == "1"),
    };
    let slot = handle.conn.clone();
    let _ = std::thread::Builder::new()
        .name("roost-introspect".into())
        .spawn(move || {
            let conn = match zbus::blocking::connection::Builder::session()
                .and_then(|b| b.serve_at(PATH, service))
                .and_then(|b| b.build())
            {
                Ok(conn) => conn,
                Err(e) => {
                    eprintln!("roost-compositor: introspect: no session bus: {e}");
                    return;
                }
            };
            let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
            match conn.request_name_with_flags(NAME, flags) {
                Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
                    eprintln!("roost-compositor: serving {NAME}");
                    let _ = slot.set(conn);
                    loop {
                        std::thread::park();
                    }
                }
                _ => eprintln!("roost-compositor: {NAME} is taken; introspection off"),
            }
        });
    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u64, app: Option<&str>, focused: bool) -> WindowSnapshot {
        WindowSnapshot {
            id,
            title: format!("w{id}"),
            app_id: app.map(str::to_owned),
            width: 640,
            height: 480,
            focused,
            hidden: false,
            x11: false,
        }
    }

    #[test]
    fn app_ids_are_desktop_ids() {
        assert_eq!(
            app_id(&window(1, Some("org.gnome.Nautilus"), false)),
            "org.gnome.Nautilus.desktop"
        );
        assert_eq!(
            app_id(&window(2, Some("firefox.desktop"), false)),
            "firefox.desktop"
        );
        assert_eq!(app_id(&window(3, None, false)), "window:3");
        assert_eq!(app_id(&window(4, Some(""), false)), "window:4");
    }

    #[test]
    fn window_properties_follow_gnome_shell() {
        let props = window_properties(&window(7, Some("org.gnome.Calculator"), true));
        let get = |k: &str| props.get(k).cloned().expect(k);
        assert_eq!(String::try_from(get("title")).unwrap(), "w7");
        assert_eq!(
            String::try_from(get("app-id")).unwrap(),
            "org.gnome.Calculator.desktop"
        );
        assert_eq!(u32::try_from(get("client-type")).unwrap(), 0);
        assert!(bool::try_from(get("has-focus")).unwrap());
        assert_eq!(u32::try_from(get("width")).unwrap(), 640);
        assert!(
            !props.contains_key("wm-class"),
            "Wayland windows have no WM class"
        );
    }

    #[test]
    fn running_apps_group_windows_and_track_focus() {
        let apps = running_apps(&[
            window(1, Some("a"), false),
            window(2, Some("a"), true),
            window(3, Some("b"), false),
        ]);
        assert_eq!(apps.len(), 2);
        let seats =
            |id: &str| Vec::<String>::try_from(apps[id]["active-on-seats"].clone()).unwrap();
        assert_eq!(seats("a.desktop"), vec!["seat0".to_owned()]);
        assert!(seats("b.desktop").is_empty());
    }
}
