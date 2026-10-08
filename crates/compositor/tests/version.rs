//! Stamped-version contract for the compositor crate's two binaries.
//!
//! The `version_tests` unit mods pin the stamp rule; this pins the wiring
//! the plan's first acceptance criterion requires: each built binary prints
//! `<name> <stamped version>` for `--version`/`-V` and exits successfully,
//! and the two side-by-side binaries agree. The stamp is `ROOST_VERSION`
//! (a `vX.Y.Z` tag or plain `X.Y.Z`) with the `v` stripped, else this
//! crate's version — the same rule the binaries bake in at compile time,
//! so build and test must share the environment (a plain `cargo test`
//! does). Uses the debug binaries cargo already built; no release build,
//! no packaging tooling.

use std::process::Command;

fn stamped_version() -> String {
    match std::env::var("ROOST_VERSION")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(tag) => tag.strip_prefix('v').unwrap_or(&tag).to_owned(),
        None => env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn expected_line(name: &str) -> String {
    let version = stamped_version();
    if name == "roost-compositor" && cfg!(feature = "night-light-vm-fixture") {
        format!("{name} {version} [night-light-vm-fixture]")
    } else {
        format!("{name} {version}")
    }
}

fn assert_reports_stamp(exe: &str, name: &str, expected: &str) {
    for flag in ["--version", "-V"] {
        let out = Command::new(exe)
            .arg(flag)
            .output()
            .unwrap_or_else(|err| panic!("{name} {flag} runs: {err}"));
        assert!(out.status.success(), "{name} {flag} must exit successfully");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim_end(),
            expected,
            "{name} {flag} must print the stamped version"
        );
    }
}

#[test]
fn compositor_version_matches_stamp() {
    let exe = env!("CARGO_BIN_EXE_roost-compositor");
    let expected = expected_line("roost-compositor");
    assert_reports_stamp(exe, "roost-compositor", &expected);
}

#[test]
fn session_launcher_version_matches_stamp() {
    let exe = env!("CARGO_BIN_EXE_roost-session");
    let expected = expected_line("roost-session");
    assert_reports_stamp(exe, "roost-session", &expected);
}

#[test]
fn side_by_side_binaries_agree_on_version() {
    let reported = ["roost-compositor", "roost-session"]
        .into_iter()
        .map(|name| {
            let var = format!("CARGO_BIN_EXE_{name}");
            let exe = std::env::var(&var).unwrap_or_else(|_| panic!("{var} is set"));
            let out = Command::new(&exe)
                .arg("--version")
                .output()
                .unwrap_or_else(|err| panic!("{name} --version runs: {err}"));
            assert!(out.status.success(), "{name} --version must succeed");
            let line = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                line.trim_end(),
                expected_line(name),
                "{name} --version must retain its exact production or fixture identity"
            );
            line.split_whitespace()
                .nth(1)
                .expect("version output has the known second version token")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reported[0], reported[1],
        "side-by-side binaries must report the same version"
    );
}
