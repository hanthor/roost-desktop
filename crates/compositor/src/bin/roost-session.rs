//! Session launcher binary (tunaOS image slice).
//!
//! `roost-session` is what greeters and `wayland-sessions/*.desktop` files
//! execute: it resolves the `roost-compositor` sibling of this binary (else a
//! `PATH` lookup) and replaces itself with it, pinning `--shell-bin` to the
//! same resolution the compositor would compute. Extra arguments pass
//! through untouched.

use std::ffi::OsString;
use std::process::ExitCode;

use roost_compositor::runtime::{current_exe_dir, resolve_shell_bin, sibling_binary};

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

/// Compositor binary for this session: sibling of the launcher when the
/// install placed them side by side, else a `PATH` lookup at exec time.
fn resolve_compositor_bin() -> std::path::PathBuf {
    if let Some(dir) = current_exe_dir() {
        if let Some(sibling) = sibling_binary(&dir, "roost-compositor") {
            return sibling;
        }
    }
    std::path::PathBuf::from("roost-compositor")
}

/// Full child command: compositor, pinned shell, then caller arguments.
fn session_argv(
    compositor: &std::path::Path,
    shell: &std::path::Path,
    extra: &[OsString],
) -> Vec<OsString> {
    let mut argv = Vec::with_capacity(extra.len() + 3);
    argv.push(compositor.as_os_str().to_owned());
    argv.push(OsString::from("--shell-bin"));
    argv.push(shell.as_os_str().to_owned());
    argv.extend(extra.iter().cloned());
    argv
}

/// `XDG_CURRENT_DESKTOP` for the session: the display manager's value
/// (from the session file's `DesktopNames=Roost;GNOME;`) when it set one,
/// else `Roost:GNOME` (greeters that skip DesktopNames, nested runs).
/// Naming GNOME second makes GNOME-only apps show, GNOME's schema
/// overrides apply, and xdg-desktop-portal pick GNOME's backend, whose
/// Shell and Mutter interfaces Roost serves, as Ubuntu's `ubuntu:GNOME`
/// does.
fn current_desktop(set: Option<&std::ffi::OsStr>) -> Option<&'static str> {
    match set {
        Some(value) if !value.is_empty() => None,
        _ => Some("Roost:GNOME"),
    }
}

fn print_help() {
    println!("roost-session: launch one Roost desktop session (image entry point)");
    println!();
    println!("Usage: roost-session [--socket NAME] [--width W] [--height H]");
    println!();
    println!("Resolves the roost-compositor sibling of this binary (else PATH)");
    println!("and replaces itself with it, pinning --shell-bin to the");
    println!("ROOST_SHELL_BIN / sibling / PATH chain. All other arguments");
    println!("pass through to roost-compositor unchanged.");
}

fn main() -> ExitCode {
    let extra: Vec<OsString> = std::env::args_os().skip(1).collect();
    if extra.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if extra.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("roost-session {}", release_version());
        return ExitCode::SUCCESS;
    }
    let compositor = resolve_compositor_bin();
    let shell = resolve_shell_bin(None);
    let argv = session_argv(&compositor, &shell, &extra);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(&argv[0]);
        command.args(&argv[1..]);
        if let Some(desktop) = current_desktop(std::env::var_os("XDG_CURRENT_DESKTOP").as_deref()) {
            command.env("XDG_CURRENT_DESKTOP", desktop);
        }
        let err = command.exec();
        eprintln!(
            "roost-session: cannot exec {}: {err}",
            argv[0].to_string_lossy()
        );
        ExitCode::FAILURE
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()
        {
            Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
            Err(err) => {
                eprintln!(
                    "roost-session: cannot run {}: {err}",
                    argv[0].to_string_lossy()
                );
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_names_gnome_after_roost() {
        assert_eq!(current_desktop(None), Some("Roost:GNOME"));
        assert_eq!(
            current_desktop(Some(std::ffi::OsStr::new(""))),
            Some("Roost:GNOME")
        );
        assert_eq!(
            current_desktop(Some(std::ffi::OsStr::new("Roost:GNOME"))),
            None
        );
        let file = include_str!("../../../../share/wayland-sessions/roost.desktop");
        assert!(file.lines().any(|l| l == "DesktopNames=Roost;GNOME;"));
    }

    #[test]
    fn argv_pins_shell_before_passthrough() {
        let argv = session_argv(
            std::path::Path::new("/usr/bin/roost-compositor"),
            std::path::Path::new("/usr/bin/roost-shell-host"),
            &[OsString::from("--socket"), OsString::from("s0")],
        );
        let text: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            text,
            vec![
                "/usr/bin/roost-compositor",
                "--shell-bin",
                "/usr/bin/roost-shell-host",
                "--socket",
                "s0",
            ]
        );
    }

    #[test]
    fn argv_without_extra_is_just_the_pin() {
        let argv = session_argv(
            std::path::Path::new("roost-compositor"),
            std::path::Path::new("roost-shell-host"),
            &[],
        );
        assert_eq!(argv.len(), 3);
    }

    #[test]
    fn sibling_binary_finds_and_rejects() {
        let dir = std::env::temp_dir().join("roost-session-test");
        std::fs::create_dir_all(&dir).expect("test dir");
        let present = dir.join("present-bin");
        std::fs::write(&present, b"x").expect("test file");
        assert_eq!(sibling_binary(&dir, "present-bin"), Some(present.clone()));
        assert_eq!(sibling_binary(&dir, "absent-bin"), None);
        std::fs::remove_file(&present).expect("cleanup");
        std::fs::remove_dir(&dir).expect("cleanup");
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
