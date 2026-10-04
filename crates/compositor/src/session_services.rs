//! Publish the hardware session's display to D-Bus and systemd activation.
//! Nested sessions never change the host user's activation environment.

use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Run a session utility with a deadline; an absent service cannot stall login.
fn run(program: &str, args: &[String]) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < TIMEOUT => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn activation_args(socket: &str, desktop: &str) -> Vec<String> {
    vec![
        "--systemd".into(),
        format!("WAYLAND_DISPLAY={socket}"),
        format!("XDG_CURRENT_DESKTOP={desktop}"),
        "XDG_SESSION_TYPE=wayland".into(),
        // Clear a previous session's X display. The portal uses Wayland.
        "DISPLAY=".into(),
    ]
}

/// Holds the systemd graphical session open for the DRM runtime's lifetime.
pub struct SessionServices {
    alive: Arc<AtomicBool>,
}
impl Drop for SessionServices {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
        let args = ["--user", "--no-block", "stop", "graphical-session.target"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let _ = run("systemctl", &args);
    }
}

/// Called only for the actual DRM backend, before its shell is started.
pub fn publish(socket: &str) -> SessionServices {
    let alive = Arc::new(AtomicBool::new(true));
    let guard = SessionServices {
        alive: alive.clone(),
    };
    let desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Roost:GNOME".into());
    if !run(
        "dbus-update-activation-environment",
        &activation_args(socket, &desktop),
    ) {
        eprintln!("roost-compositor: session services: display environment import failed");
        return guard;
    }
    eprintln!("roost-compositor: session services: display environment imported");
    // GTK can activate portals before the display is ready. After the shell
    // owns its bus name, clear any start-limit failure and refresh the portal
    // frontend so it chooses Roost's backends using the imported desktop.
    std::thread::spawn(move || {
        let args: Vec<String> = [
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.GetNameOwner",
            "org.gnome.Shell",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let start = Instant::now();
        while alive.load(Ordering::Acquire) && start.elapsed() < Duration::from_secs(15) {
            if run("gdbus", &args) {
                // Tell GDM the session and display are ready rather than
                // letting its timed non-GNOME fallback hand off plymouth.
                let mut registration = [
                    "call",
                    "--system",
                    "--dest",
                    "org.gnome.DisplayManager",
                    "--object-path",
                    "/org/gnome/DisplayManager/Manager",
                    "--method",
                    "org.gnome.DisplayManager.Manager.RegisterSession",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                if run("gdbus", &registration) {
                    *registration.last_mut().unwrap() =
                        "org.gnome.DisplayManager.Manager.RegisterDisplay".into();
                    if run("gdbus", &registration) {
                        eprintln!("roost-compositor: session services: GDM session and display registered");
                    } else {
                        eprintln!(
                            "roost-compositor: session services: GDM display registration failed"
                        );
                    }
                }

                if !alive.load(Ordering::Acquire) {
                    return;
                }
                let start_graphical = ["--user", "start", "graphical-session.target"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if !run("systemctl", &start_graphical) {
                    eprintln!("roost-compositor: session services: graphical target failed");
                    return;
                }
                // Drop may race the worker's start. Stop again if shutdown
                // occurred before systemd acknowledged the target.
                if !alive.load(Ordering::Acquire) {
                    let stop = ["--user", "--no-block", "stop", "graphical-session.target"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let _ = run("systemctl", &stop);
                    return;
                }
                eprintln!("roost-compositor: session services: graphical target active");

                let reset = [
                    "--user",
                    "reset-failed",
                    "xdg-desktop-portal-gtk.service",
                    "xdg-desktop-portal-gnome.service",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                let _ = run("systemctl", &reset);
                let restart = [
                    "--user",
                    "--no-block",
                    "restart",
                    "xdg-desktop-portal.service",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
                if run("systemctl", &restart) {
                    eprintln!("roost-compositor: session services: portals refreshed");
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("roost-compositor: session services: shell bus name not ready");
    });
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_import_names_only_the_hardware_display() {
        let args = activation_args("wayland-roost", "Roost:GNOME");
        assert_eq!(
            args,
            [
                "--systemd",
                "WAYLAND_DISPLAY=wayland-roost",
                "XDG_CURRENT_DESKTOP=Roost:GNOME",
                "XDG_SESSION_TYPE=wayland",
                "DISPLAY="
            ]
        );
        assert!(!args.iter().any(|a| a == "--all"));
    }

    #[test]
    fn session_utility_failures_are_reported() {
        assert!(!run("/definitely/missing/roost-session-tool", &[]));
        assert!(!run("/bin/false", &[]));
        assert!(run("/bin/true", &[]));
    }
}
