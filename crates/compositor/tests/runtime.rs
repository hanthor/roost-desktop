//! T1 nested runtime loop + launch tests.
//!
//! Covers `tuna_compositor::runtime` without window mapping or input
//! (T2 scope): env hygiene, default session identity, and a live launch
//! smoke test that skips cleanly when no host display/EGL exists.
//!
//! Conventions follow the sibling harnesses (`handshake.rs`,
//! `recovery.rs`): bounded retry loops, plain assert macros. All code
//! here is original.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tuna_compositor::runtime::{apply_nested_env, restore_env, NestedSession};

const RETRY_ROUNDS: usize = 500;
const RETRY_SLEEP: Duration = Duration::from_millis(5);

/// Path of the sibling `tuna-compositor` binary: the test executable lives
/// in `target/<profile>/deps/`, the binary in `target/<profile>/`.
fn compositor_bin() -> std::path::PathBuf {
    let mut dir = std::env::current_exe().expect("test executable path");
    dir.pop();
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir.pop();
    }
    dir.join("tuna-compositor")
}

/// Env hygiene in ONE test, run sequentially: process env is global, so
/// splitting set/restore across tests would race with each other.
/// `apply_nested_env` points `WAYLAND_DISPLAY` at our socket and returns
/// the previous value; `restore_env` puts it back, or removes the var
/// when there was none.
#[test]
fn nested_env_apply_returns_previous_and_restore_reverts() {
    // Preserve whatever the ambient test process had; restored at the end
    // so no other test in this binary observes our mutation.
    let outer = std::env::var_os("WAYLAND_DISPLAY");

    // Case 1: a previous value exists.
    std::env::set_var("WAYLAND_DISPLAY", "host-display-0");
    let prev = apply_nested_env("tuna-nested-test");
    assert_eq!(
        prev.as_deref(),
        Some(std::ffi::OsStr::new("host-display-0"))
    );
    assert_eq!(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        Some(std::ffi::OsStr::new("tuna-nested-test"))
    );
    restore_env(prev);
    assert_eq!(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        Some(std::ffi::OsStr::new("host-display-0"))
    );

    // Case 2: no previous value.
    std::env::remove_var("WAYLAND_DISPLAY");
    let prev = apply_nested_env("tuna-nested-test");
    assert_eq!(prev, None);
    assert_eq!(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        Some(std::ffi::OsStr::new("tuna-nested-test"))
    );
    restore_env(prev);
    assert_eq!(std::env::var_os("WAYLAND_DISPLAY"), None);

    // Leave the process env as we found it.
    restore_env(outer);
}

/// `default_for_pid` names the socket after our pid and picks a positive
/// default output size.
#[test]
fn default_session_names_socket_after_pid_with_positive_size() {
    let session = NestedSession::default_for_pid();
    let pid = std::process::id().to_string();
    assert!(
        session.socket_name.contains(&pid),
        "socket name {:?} does not contain pid {pid}",
        session.socket_name
    );
    assert!(session.width > 0, "default width must be positive");
    assert!(session.height > 0, "default height must be positive");
}

/// Live launch smoke test: run the real binary (which drives
/// `Runtime::launch` on its main thread) with a unique `--socket` name.
/// Without a host display/EGL the child exits with a
/// `RuntimeError::Backend` diagnostic — a skip, not a failure. On success
/// the private socket file must appear; killing the child then shuts the
/// session down cleanly.
///
/// NOTE: this deliberately does NOT call `Runtime::launch` in-process.
/// `winit::init` asserts it runs on the main thread and panics on any
/// `cargo test` worker thread, so the success path would be unreachable
/// in-process. The child process exercises the genuine main-thread path.
#[test]
fn launch_smoke_binds_private_socket_or_skips_without_backend() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let socket_name = format!("tuna-test-{}-{nanos}", std::process::id());
    let bin = compositor_bin();
    assert!(
        bin.is_file(),
        "missing sibling binary {}; run `cargo build -p tuna-compositor --bins` first",
        bin.display()
    );
    let mut child = Command::new(bin)
        .args([
            "--socket",
            &socket_name,
            "--width",
            "800",
            "--height",
            "600",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn nested compositor");

    // `ListeningSocketSource::with_name` binds under `XDG_RUNTIME_DIR`.
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let socket_path = runtime_dir.join(&socket_name);
    let mut bound = false;
    let mut exited = None;
    for _ in 0..RETRY_ROUNDS {
        if socket_path.exists() {
            bound = true;
            break;
        }
        match child.try_wait().expect("poll nested compositor") {
            Some(status) => {
                exited = Some(status);
                break;
            }
            None => std::thread::sleep(RETRY_SLEEP),
        }
    }

    if bound {
        assert!(
            socket_path.exists(),
            "private socket {} vanished",
            socket_path.display()
        );
        child.kill().expect("stop nested compositor");
        child.wait().expect("reap nested compositor");
        // Best-effort cleanup of our uniquely named socket.
        let _ = std::fs::remove_file(&socket_path);
        return;
    }

    // The child exited before binding: only acceptable when the backend
    // itself is unavailable in this environment (headless CI).
    let status = match exited {
        Some(status) => status,
        None => {
            child.kill().expect("stop hung nested compositor");
            child.wait().expect("reap nested compositor")
        }
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(
        stderr.contains("nested backend unavailable"),
        "nested compositor exited ({status}) without binding: {stderr}"
    );
    eprintln!("SKIP: no host display/EGL for nested launch ({status}): {stderr}");
}

#[test]
fn backend_choice_parses_and_auto_resolves_by_host_display() {
    use tuna_compositor::runtime::BackendChoice;
    assert_eq!(BackendChoice::parse("auto"), Some(BackendChoice::Auto));
    assert_eq!(BackendChoice::parse("nested"), Some(BackendChoice::Winit));
    assert_eq!(BackendChoice::parse("kms"), Some(BackendChoice::Drm));
    assert_eq!(BackendChoice::parse("vulkan"), None);
    // A host display always means nested.
    assert_eq!(BackendChoice::Auto.resolve(true), BackendChoice::Winit);
    // No host display (greetd on a TTY): hardware when built with it.
    let bare = BackendChoice::Auto.resolve(false);
    if cfg!(feature = "drm") {
        assert_eq!(bare, BackendChoice::Drm);
    } else {
        assert_eq!(bare, BackendChoice::Winit);
    }
    // Explicit choices are never overridden.
    assert_eq!(BackendChoice::Drm.resolve(true), BackendChoice::Drm);
    assert_eq!(BackendChoice::Winit.resolve(false), BackendChoice::Winit);
}
