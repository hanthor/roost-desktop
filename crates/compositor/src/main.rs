//! Nested compositor binary (001 T1).
//!
//! Starts one isolated nested Wayland session on a private socket and
//! drives it until the window closes. The host display session is never
//! touched: `WAYLAND_DISPLAY` is set in this process only, and shutdown
//! restores the previous value.

use std::process::ExitCode;

use rwd_compositor::runtime::{run, NestedSession};

/// `rwd-compositor [--socket NAME] [--width W] [--height H] [--shell-bin PATH]`.
fn main() -> ExitCode {
    let mut session = NestedSession::default_for_pid();
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
            "--help" | "-h" => {
                println!("rwd-compositor: nested RWD session (001 developer preview)");
                println!();
                println!(
                    "Usage: rwd-compositor [--socket NAME] [--width W] [--height H] [--shell-bin PATH]"
                );
                println!();
                println!("Starts one isolated nested Wayland session on a private");
                println!("socket (default rwd-nested-<pid>) and drives it until the");
                println!("window closes. Needs a host Wayland/X session with EGL.");
                println!("The shell binary (default: RWD_SHELL_BIN, else the");
                println!("rwd-shell-host sibling) is spawned supervised with a");
                println!("finite restart budget; WAYLAND_DISPLAY is set for this");
                println!("process only and restored on shutdown.");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("rwd-compositor: unknown argument {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!(
        "rwd-compositor: nested session on {} ({}x{})",
        session.socket_name, session.width, session.height
    );
    match run(&session) {
        Ok(stats) => {
            println!(
                "rwd-compositor: shutdown after {} frames, {} clients, {} shell restarts",
                stats.frames, stats.clients, stats.shell_restarts
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("rwd-compositor: {err}");
            ExitCode::FAILURE
        }
    }
}
