//! Nested compositor binary (001 T1).
//!
//! Starts one isolated nested Wayland session on a private socket and
//! drives it until the window closes. The host display session is never
//! touched: `WAYLAND_DISPLAY` is set in this process only, and shutdown
//! restores the previous value.

use std::process::ExitCode;

use roost_compositor::runtime::{run, NestedSession};

/// `roost-compositor [--socket NAME] [--width W] [--height H] [--shell-bin PATH] [--xwayland]`.
/// `ROOST_XWAYLAND=1` also opts in to X11 compatibility.
///
/// Release version stamped at build time: `ROOST_VERSION` (a `vX.Y.Z` tag or
/// plain `X.Y.Z`) wins, otherwise the crate version. Duplicated per binary on
/// purpose — no shared dependency for a few lines.
fn normalize_version<'a>(raw: Option<&'a str>, fallback: &'a str) -> &'a str {
    match raw {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => fallback,
    }
}

fn release_version() -> &'static str {
    normalize_version(option_env!("ROOST_VERSION"), env!("CARGO_PKG_VERSION"))
}

fn main() -> ExitCode {
    let mut session = NestedSession::default_for_pid();
    if std::env::var("ROOST_XWAYLAND").is_ok_and(|value| value == "1") {
        session.xwayland = true;
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
            "--version" | "-V" => {
                println!("roost-compositor {}", release_version());
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("roost-compositor: nested Roost session (001 developer preview)");
                println!();
                println!(
                    "Usage: roost-compositor [--socket NAME] [--width W] [--height H] [--shell-bin PATH] [--xwayland]"
                );
                println!();
                println!("Starts one isolated nested Wayland session on a private");
                println!("socket (default roost-nested-<pid>) and drives it until the");
                println!("window closes. Needs a host Wayland/X session with EGL.");
                println!("The shell binary (default: ROOST_SHELL_BIN, else the");
                println!("roost-shell-host sibling) is spawned supervised with a");
                println!("finite restart budget; WAYLAND_DISPLAY is set for this");
                println!("process only and restored on shutdown.");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("roost-compositor: unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!(
        "roost-compositor: nested session on {} ({}x{})",
        session.socket_name, session.width, session.height
    );
    match run(&session) {
        Ok(stats) => {
            println!(
                "roost-compositor: shutdown after {} frames, {} clients, {} shell restarts",
                stats.frames, stats.clients, stats.shell_restarts
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("roost-compositor: {err}");
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
