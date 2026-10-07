//! GNOME Screen Reader activation for the real hardware session.
//! The distribution's Orca service owns restart and shutdown. Nested shells
//! never connect to the user's service manager, even with preview UI overrides.

use gio::prelude::*;
use glib::variant::ToVariant;
use std::cell::Cell;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

const DEST: &str = "org.freedesktop.systemd1";
const ROOT: &str = "/org/freedesktop/systemd1";
const TARGET: &str = "/org/freedesktop/systemd1/unit/roost_2dsession_2etarget";
const ORCA: &str = "/org/freedesktop/systemd1/unit/orca_2eservice";
const UNIT: &str = "org.freedesktop.systemd1.Unit";
const SERVICE: &str = "org.freedesktop.systemd1.Service";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";
const KEY: &str = "screen-reader-enabled";
const ENV_LIMIT: usize = 128 * 1024;
thread_local! {
    static STARTED: Cell<bool> = const { Cell::new(false) };
}

fn hardware(marker: Option<&str>) -> bool {
    marker == Some("hardware")
}

fn display_matches(environment: &[u8], display: &str) -> bool {
    let mut entries = environment
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"WAYLAND_DISPLAY="));
    entries.next() == Some(display.as_bytes()) && entries.next().is_none()
}

fn owns_process(pid: u32, display: &str) -> bool {
    if pid == 0 {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(format!("/proc/{pid}/environ")) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if metadata.uid() != unsafe { libc::geteuid() } {
        return false;
    }
    let mut environment = Vec::new();
    if (&mut file)
        .take((ENV_LIMIT + 1) as u64)
        .read_to_end(&mut environment)
        .is_err()
        || environment.len() > ENV_LIMIT
    {
        return false;
    }
    display_matches(&environment, display)
}

async fn property(
    connection: &gio::DBusConnection,
    path: &str,
    interface: &str,
    name: &str,
) -> Result<glib::Variant, String> {
    connection
        .call_future(
            Some(DEST),
            path,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(interface, name).to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            2_000,
        )
        .await
        .map_err(|e| e.to_string())?
        .get::<(glib::Variant,)>()
        .map(|(value,)| value)
        .ok_or_else(|| "service manager returned an invalid property".into())
}

/// Reconcile only inside the compositor-owned hardware graphical session.
/// A missing schema, service or user manager leaves the shell usable and
/// reports failure; it never falls back to killing or replacing a process.
pub fn start() {
    if !hardware(std::env::var("ROOST_SESSION_SERVICES").ok().as_deref()) {
        return;
    }
    let Some(settings) = crate::settings("org.gnome.desktop.a11y.applications") else {
        return;
    };
    if !settings
        .settings_schema()
        .is_some_and(|schema| schema.has_key(KEY))
    {
        return;
    }
    let Ok(display) = std::env::var("WAYLAND_DISPLAY") else {
        return;
    };
    if display.is_empty() {
        return;
    }
    if STARTED.with(|started| started.replace(true)) {
        return;
    }
    // Register interest in changes while retaining the settings object in the
    // task. Each iteration reads the latest value, coalescing rapid toggles.
    settings.connect_changed(Some(KEY), |_, _| {});
    glib::MainContext::default().spawn_local(async move {
        let connection = match gio::bus_get_future(gio::BusType::Session).await {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("roost-shell-gtk: Orca service manager unavailable: {error}");
                return;
            }
        };
        if let Err(error) = connection
            .call_future(
                Some(DEST),
                ROOT,
                MANAGER,
                "LoadUnit",
                Some(&("orca.service",).to_variant()),
                glib::VariantTy::new("(o)").ok(),
                gio::DBusCallFlags::NONE,
                2_000,
            )
            .await
        {
            eprintln!("roost-shell-gtk: Orca service unavailable: {error}");
            return;
        }
        let mut last_error = None;
        let mut requested: Option<(bool, Instant)> = None;
        loop {
            let result = async {
                let target = property(&connection, TARGET, UNIT, "ActiveState").await?;
                if target.str() != Some("active") {
                    // The compositor imports the display before starting this
                    // target. Compositor cleanup stops only our owned Orca.
                    return Ok(());
                }
                let wanted = settings.boolean(KEY);
                if wanted {
                    if let Some(interface) = crate::settings("org.gnome.desktop.interface") {
                        if !interface.boolean("toolkit-accessibility") {
                            interface
                                .set_boolean("toolkit-accessibility", true)
                                .map_err(|e| e.to_string())?;
                        }
                    }
                }
                let active = property(&connection, ORCA, UNIT, "ActiveState").await?;
                let state = active.str().ok_or("invalid Orca state")?;
                let pid = property(&connection, ORCA, SERVICE, "MainPID")
                    .await?
                    .get::<u32>()
                    .ok_or("invalid Orca PID")?;
                if (pid != 0 && !owns_process(pid, &display)) || (state == "active" && pid == 0) {
                    return Err("Orca belongs to another display; leaving it untouched".into());
                }
                if matches!(state, "activating" | "deactivating" | "reloading") {
                    return Ok(());
                }
                if wanted == (state == "active") {
                    requested = None;
                    return Ok(());
                }
                if wanted {
                    // The distribution unit uses Orca's --replace contract.
                    // Refuse to activate it over any separately launched Orca;
                    // an inactive unit does not establish process ownership.
                    let existing = connection
                        .call_future(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "NameHasOwner",
                            Some(&("org.gnome.Orca1.Service",).to_variant()),
                            glib::VariantTy::new("(b)").ok(),
                            gio::DBusCallFlags::NONE,
                            2_000,
                        )
                        .await
                        .map_err(|e| e.to_string())?
                        .get::<(bool,)>()
                        .ok_or("invalid Orca owner reply")?
                        .0;
                    if existing {
                        return Err("separately launched Orca left untouched".into());
                    }
                    let environment = property(&connection, ROOT, MANAGER, "Environment")
                        .await?
                        .get::<Vec<String>>()
                        .ok_or("invalid activation environment")?;
                    let matches: Vec<_> = environment
                        .iter()
                        .filter_map(|entry| entry.strip_prefix("WAYLAND_DISPLAY="))
                        .collect();
                    if matches != [display.as_str()] {
                        return Err("activation display changed; leaving Orca untouched".into());
                    }
                }
                if requested.as_ref().is_some_and(|(previous, at)| {
                    *previous == wanted && at.elapsed() < Duration::from_secs(5)
                }) {
                    return Ok(());
                }
                let method = if wanted { "StartUnit" } else { "StopUnit" };
                connection
                    .call_future(
                        Some(DEST),
                        ROOT,
                        MANAGER,
                        method,
                        Some(&("orca.service", "replace").to_variant()),
                        glib::VariantTy::new("(o)").ok(),
                        gio::DBusCallFlags::NONE,
                        5_000,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                requested = Some((wanted, Instant::now()));
                eprintln!("roost-shell-gtk: Orca {method} requested for hardware display");
                Ok::<(), String>(())
            }
            .await;
            let error = result.err();
            if error != last_error {
                if let Some(error) = &error {
                    eprintln!("roost-shell-gtk: Orca activation unavailable: {error}");
                }
                last_error = error;
            }
            glib::timeout_future(Duration::from_secs(1)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_actual_hardware_marker_enables_service_control() {
        for marker in [None, Some("nested"), Some(""), Some("Hardware")] {
            assert!(!hardware(marker));
        }
        assert!(hardware(Some("hardware")));
    }

    #[test]
    fn process_display_must_match_once_and_exactly() {
        assert!(display_matches(
            b"OTHER=x\0WAYLAND_DISPLAY=wayland-roost\0",
            "wayland-roost"
        ));
        for env in [
            b"WAYLAND_DISPLAY=wayland-host\0".as_slice(),
            b"WAYLAND_DISPLAY=wayland-roost-other\0".as_slice(),
            b"WAYLAND_DISPLAY=wayland-roost\0WAYLAND_DISPLAY=wayland-host\0".as_slice(),
            b"OTHER=WAYLAND_DISPLAY=wayland-roost\0".as_slice(),
        ] {
            assert!(!display_matches(env, "wayland-roost"));
        }
    }
}
