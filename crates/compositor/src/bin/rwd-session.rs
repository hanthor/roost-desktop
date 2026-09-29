//! Session launcher binary (tunaOS image slice).
//!
//! `rwd-session` is what greeters and `wayland-sessions/*.desktop` files
//! execute: it resolves the `rwd-compositor` sibling of this binary (else a
//! `PATH` lookup) and replaces itself with it, pinning `--shell-bin` to the
//! same resolution the compositor would compute. Extra arguments pass
//! through untouched.

use std::ffi::OsString;
use std::process::ExitCode;

use rwd_compositor::runtime::{current_exe_dir, resolve_shell_bin, sibling_binary};

/// Compositor binary for this session: sibling of the launcher when the
/// install placed them side by side, else a `PATH` lookup at exec time.
fn resolve_compositor_bin() -> std::path::PathBuf {
    if let Some(dir) = current_exe_dir() {
        if let Some(sibling) = sibling_binary(&dir, "rwd-compositor") {
            return sibling;
        }
    }
    std::path::PathBuf::from("rwd-compositor")
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

fn print_help() {
    println!("rwd-session: launch one RWD desktop session (image entry point)");
    println!();
    println!("Usage: rwd-session [--socket NAME] [--width W] [--height H]");
    println!();
    println!("Resolves the rwd-compositor sibling of this binary (else PATH)");
    println!("and replaces itself with it, pinning --shell-bin to the");
    println!("RWD_SHELL_BIN / sibling / PATH chain. All other arguments");
    println!("pass through to rwd-compositor unchanged.");
}

fn main() -> ExitCode {
    let extra: Vec<OsString> = std::env::args_os().skip(1).collect();
    if extra.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }
    let compositor = resolve_compositor_bin();
    let shell = resolve_shell_bin(None);
    let argv = session_argv(&compositor, &shell, &extra);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&argv[0]).args(&argv[1..]).exec();
        eprintln!(
            "rwd-session: cannot exec {}: {err}",
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
                    "rwd-session: cannot run {}: {err}",
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
    fn argv_pins_shell_before_passthrough() {
        let argv = session_argv(
            std::path::Path::new("/usr/bin/rwd-compositor"),
            std::path::Path::new("/usr/bin/rwd-shell-host"),
            &[OsString::from("--socket"), OsString::from("s0")],
        );
        let text: Vec<String> = argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            text,
            vec![
                "/usr/bin/rwd-compositor",
                "--shell-bin",
                "/usr/bin/rwd-shell-host",
                "--socket",
                "s0",
            ]
        );
    }

    #[test]
    fn argv_without_extra_is_just_the_pin() {
        let argv = session_argv(
            std::path::Path::new("rwd-compositor"),
            std::path::Path::new("rwd-shell-host"),
            &[],
        );
        assert_eq!(argv.len(), 3);
    }

    #[test]
    fn sibling_binary_finds_and_rejects() {
        let dir = std::env::temp_dir().join("rwd-session-test");
        std::fs::create_dir_all(&dir).expect("test dir");
        let present = dir.join("present-bin");
        std::fs::write(&present, b"x").expect("test file");
        assert_eq!(sibling_binary(&dir, "present-bin"), Some(present.clone()));
        assert_eq!(sibling_binary(&dir, "absent-bin"), None);
        std::fs::remove_file(&present).expect("cleanup");
        std::fs::remove_dir(&dir).expect("cleanup");
    }
}
