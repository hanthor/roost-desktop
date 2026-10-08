//! Nested compositor binary (001 T1).
//!
//! Starts one isolated nested Wayland session on a private socket and
//! drives it until the window closes. The host display session is never
//! touched: `WAYLAND_DISPLAY` is set in this process only, and shutdown
//! restores the previous value.

use std::process::ExitCode;

use tuna_compositor::runtime::{run, BackendChoice, NestedSession};

/// `tuna-compositor [--socket NAME] [--width W] [--height H] [--shell-bin PATH] [--xwayland]`.
/// X11 compatibility is on when `Xwayland` is installed (`TUNA_XWAYLAND=0`
/// turns it off, `--xwayland` or `TUNA_XWAYLAND=1` forces it).
///
/// Release version stamped at build time: `TUNA_VERSION` (a `vX.Y.Z` tag or
/// plain `X.Y.Z`) wins, otherwise the crate version. Duplicated per binary on
/// purpose — no shared dependency for a few lines.
fn normalize_version<'a>(raw: Option<&'a str>, fallback: &'a str) -> &'a str {
    match raw {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => fallback,
    }
}

fn release_version() -> &'static str {
    normalize_version(option_env!("TUNA_VERSION"), env!("CARGO_PKG_VERSION"))
}

fn main() -> ExitCode {
    // The old name's environment and per-user directories, for one release (#505).
    tuna_shell_control::legacy::import_env("tuna-compositor");
    tuna_shell_control::legacy::adopt_dirs("tuna-compositor");
    let mut session = NestedSession::default_for_pid();
    session.xwayland = tuna_compositor::runtime::xwayland_wanted(
        std::env::var_os("TUNA_XWAYLAND"),
        std::env::var_os("PATH"),
    );
    // Output scale (#59): TUNA_SCALE=1.5, or --scale.
    if let Some(scale) = std::env::var("TUNA_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        session.scale = scale;
    }
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                if let Some(name) = args.next() {
                    session.socket_name = name;
                }
            }
            "--width" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    session.width = value;
                }
            }
            "--scale" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    session.scale = value;
                }
            }
            "--height" => {
                if let Some(value) = args.next().and_then(|v| v.parse().ok()) {
                    session.height = value;
                }
            }
            "--shell-bin" => {
                if let Some(path) = args.next() {
                    session.shell_bin = Some(path.into());
                }
            }
            "--xwayland" => {
                session.xwayland = true;
            }
            "--startup-overview" => {
                session.startup_overview = true;
            }
            "--backend" => match args.next().as_deref().and_then(BackendChoice::parse) {
                Some(choice) => session.backend = choice,
                None => {
                    eprintln!("tuna-compositor: --backend takes auto, winit, or drm");
                    return ExitCode::FAILURE;
                }
            },
            "--version" | "-V" => {
                println!("tuna-compositor {}", release_version());
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("tuna-compositor: nested Tuna Desktop session (001 developer preview)");
                println!();
                println!(
                    "Usage: tuna-compositor [--backend auto|winit|drm] [--socket NAME] [--width W] [--height H] [--shell-bin PATH] [--xwayland] [--startup-overview]"
                );
                println!();
                println!("Starts one isolated nested Wayland session on a private");
                println!("socket (default tuna-nested-<pid>) and drives it until the");
                println!("window closes. --backend auto (default) runs nested when a");
                println!("host Wayland/X display exists and as a DRM/KMS hardware");
                println!("session otherwise (from a TTY via greetd; needs a seat");
                println!("from logind or seatd). --width/--height apply to nested runs.");
                println!("The shell binary (default: TUNA_SHELL_BIN, else the");
                println!("tuna-shell-host sibling) is spawned supervised with a");
                println!("finite restart budget; WAYLAND_DISPLAY is set for this");
                println!("process only and restored on shutdown.");
                println!("--startup-overview opens the overview at start, as GNOME");
                println!("does at login (tuna-session passes it).");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("tuna-compositor: unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!(
        "tuna-compositor: nested session on {} ({}x{})",
        session.socket_name, session.width, session.height
    );
    match run(&session) {
        Ok(stats) => {
            println!(
                "tuna-compositor: shutdown after {} frames, {} clients, {} shell restarts",
                stats.frames, stats.clients, stats.shell_restarts
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("tuna-compositor: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod version_tests {
    use super::normalize_version;

    #[test]
    fn tag_prefix_is_stripped() {
        assert_eq!(normalize_version(Some("v1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn plain_version_is_kept() {
        assert_eq!(normalize_version(Some("1.2.3"), "0.1.0"), "1.2.3");
    }

    #[test]
    fn missing_or_empty_falls_back() {
        assert_eq!(normalize_version(None, "0.1.0"), "0.1.0");
        assert_eq!(normalize_version(Some(""), "0.1.0"), "0.1.0");
    }
}
