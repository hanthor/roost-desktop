//! Static release contract: the plan criteria that need no build and no
//! package tooling.
//!
//! `scripts/roost-release` must stay an executable fail-fast script that
//! stamps `ROOST_VERSION`, stages the four binaries plus the session entry,
//! generates the control metadata from the stamp, and builds via
//! `dpkg-deb`. `docs/release.md` must keep the checklist and the
//! notes-template sections; `docs/install.md` must keep the package-install
//! path with its upgrade/uninstall data-preservation notes. The full
//! end-to-end proof stays in CI and `scripts/check-release-package`.
//! Follows the `launch_script.rs` pin-the-contract pattern.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}

fn read_workspace(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{rel} reads: {err}"))
}

#[test]
fn release_script_is_an_executable_fail_fast_script() {
    let rel = "scripts/roost-release";
    let meta = std::fs::metadata(workspace_root().join(rel)).expect("release script exists");
    #[cfg(unix)]
    assert_ne!(
        std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111,
        0,
        "release script must be executable"
    );

    let status = std::process::Command::new("sh")
        .arg("-n")
        .arg(workspace_root().join(rel))
        .status()
        .expect("sh runs");
    assert!(status.success(), "release script passes sh -n");

    let body = read_workspace(rel);
    assert!(body.contains("set -eu"), "release script must fail fast");
}

#[test]
fn release_script_stages_layout_and_metadata_from_the_stamp() {
    let body = read_workspace("scripts/roost-release");
    for token in [
        "ROOST_VERSION",
        "roost-compositor",
        "roost-session",
        "roost-shell-host",
        "roost-greeter",
        "roost.desktop",
        "DEBIAN/control",
        "Version:",
        "Depends:",
        "dpkg-deb",
        "--version",
    ] {
        assert!(
            body.contains(token),
            "release script must derive the package from the stamp ({token})"
        );
    }
}

#[test]
fn release_notes_template_keeps_the_required_sections() {
    let doc = read_workspace("docs/release.md");
    for token in [
        "## Checklist",
        "scripts/roost-release",
        "git tag vX.Y.Z",
        "## Release notes template",
        "## Changes",
        "## Known limitations",
        "## Supported configurations",
        "## Upgrade and uninstall",
    ] {
        assert!(
            doc.contains(token),
            "release doc must keep the runbook shape ({token})"
        );
    }
}

#[test]
fn install_doc_covers_package_install_and_data_preservation() {
    let doc = read_workspace("docs/install.md");
    for token in [
        "apt install",
        "Dependencies resolve automatically",
        "login screen",
        "Upgrading",
        "removing the package",
        "preserv",
    ] {
        assert!(
            doc.contains(token),
            "install doc must cover the package flow ({token})"
        );
    }
}

#[test]
fn release_requires_a_filled_visual_review() {
    let script = read_workspace("scripts/roost-release");
    for token in ["docs/reviews/$RELEASE_TAG.md", "TBD"] {
        assert!(
            script.contains(token),
            "release script checks review: {token}"
        );
    }
    let template = read_workspace("docs/reviews/TEMPLATE.md");
    for section in [
        "## Reviewer",
        "## Inputs",
        "## Journeys",
        "## Sign-off",
        "marlin:gnome",
    ] {
        assert!(
            template.contains(section),
            "review template keeps {section}"
        );
    }
    assert!(
        read_workspace("docs/release.md").contains("docs/reviews/vX.Y.Z.md"),
        "release checklist names the review step"
    );
}
