//! Stamped-version contract for the shell-host binary.
//!
//! The `version_tests` unit mod pins the stamp rule; this pins the wiring
//! the plan's first acceptance criterion requires: the built binary prints
//! `tuna-shell-host <stamped version>` for `--version`/`-V` and exits
//! successfully. The stamp is `TUNA_VERSION` (a `vX.Y.Z` tag or plain
//! `X.Y.Z`) with the `v` stripped, else this crate's version — the same
//! rule the binary bakes in at compile time, so build and test must share
//! the environment (a plain `cargo test` does). Uses the debug binary
//! cargo already built; no release build, no packaging tooling.

use std::process::Command;

fn stamped_version() -> String {
    match std::env::var("TUNA_VERSION").ok().filter(|v| !v.is_empty()) {
        Some(tag) => tag.strip_prefix('v').unwrap_or(&tag).to_owned(),
        None => env!("CARGO_PKG_VERSION").to_owned(),
    }
}

#[test]
fn shell_host_version_matches_stamp() {
    let exe = env!("CARGO_BIN_EXE_tuna-shell-host");
    let expected = format!("tuna-shell-host {}", stamped_version());
    for flag in ["--version", "-V"] {
        let out = Command::new(exe)
            .arg(flag)
            .output()
            .unwrap_or_else(|err| panic!("tuna-shell-host {flag} runs: {err}"));
        assert!(
            out.status.success(),
            "tuna-shell-host {flag} must exit successfully"
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            expected,
            "tuna-shell-host {flag} must print the stamped version"
        );
    }
}
