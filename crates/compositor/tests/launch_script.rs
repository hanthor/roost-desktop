//! T6 launch-command reproducibility check.
//!
//! The documented one-command journey (`scripts/tuna-nested`, described
//! in `docs/nested-session.md`) is a shell script, so this test pins
//! its contract instead of executing a nested session: the script
//! exists, is executable, passes `sh -n`, and documents the crash
//! journey verbs; the doc names the script and the artifact layout.
//! Live execution stays environment-gated (needs host EGL); the
//! headless equivalent is the 100-run harness in `recovery.rs`.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}

#[test]
fn launch_script_reproduces_the_documented_journey() {
    let script = workspace_root().join("scripts").join("tuna-nested");
    let meta = std::fs::metadata(&script).expect("launcher script exists");
    #[cfg(unix)]
    assert_ne!(
        std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111,
        0,
        "launcher must be executable"
    );

    let status = std::process::Command::new("sh")
        .arg("-n")
        .arg(&script)
        .status()
        .expect("sh runs");
    assert!(status.success(), "launcher passes sh -n");

    let body = std::fs::read_to_string(&script).expect("launcher reads");
    for token in [
        "tuna-compositor",
        "--shell-bin",
        "run",
        "shell-pid",
        "kill-shell",
        "nested.log",
        "compositor.pid",
    ] {
        assert!(
            body.contains(token),
            "launcher must document the journey verb {token}"
        );
    }

    let doc = std::fs::read_to_string(workspace_root().join("docs").join("nested-session.md"))
        .expect("journey doc exists");
    for token in [
        "scripts/tuna-nested run",
        "kill-shell",
        "WAYLAND_DISPLAY",
        "TUNA_CONTROL_SOCKET",
        "nested.log",
    ] {
        assert!(doc.contains(token), "journey doc must cover {token}");
    }
}
