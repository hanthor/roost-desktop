//! Static release contract: the plan criteria that need no build and no
//! package tooling.
//!
//! `scripts/tuna-release` must stay an executable fail-fast script that
//! stamps `TUNA_VERSION`, stages the six binaries plus the session entry,
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
    let rel = "scripts/tuna-release";
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
    let body = read_workspace("scripts/tuna-release");
    for token in [
        "TUNA_VERSION",
        "tuna-compositor",
        "tuna-session",
        "tuna-shell-gtk",
        "tuna-shell-host",
        "tuna-ibus-bridge",
        "tuna-greeter",
        "tuna.desktop",
        "pam.d/tuna-lock",
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
fn debian_archive_is_root_owned_and_ships_the_gtk_shell() {
    // #476: system files must not keep the build user's uid/gid.
    let release = read_workspace("scripts/tuna-release");
    assert!(
        release.contains("dpkg-deb --root-owner-group --build"),
        "release script must normalize archive ownership to root"
    );
    // #468: the preferred GTK shell is listed, owned and installed cleanly.
    let check = read_workspace("scripts/check-deb-clean-install");
    for token in [
        "dpkg-deb -c",
        "root/root",
        "./usr/bin/tuna-shell-gtk",
        "apt-get install",
    ] {
        assert!(check.contains(token), "clean-install check keeps {token}");
    }
    let ci = read_workspace(".github/workflows/ci.yml");
    assert!(
        ci.contains("scripts/check-deb-clean-install package-artifacts/*.deb"),
        "CI must install the built .deb on a clean image"
    );
}

#[test]
fn release_notes_template_keeps_the_required_sections() {
    let doc = read_workspace("docs/release.md");
    for token in [
        "## Checklist",
        "scripts/tuna-release",
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
    let script = read_workspace("scripts/tuna-release");
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

#[test]
fn arch_package_stamps_and_checks_the_version() {
    let pkgbuild = read_workspace("packaging/arch/PKGBUILD");
    for token in [
        "TUNA_VERSION",
        "tuna-compositor",
        "tuna-session",
        "tuna-shell-host",
        "tuna-shell-gtk",
        "tuna-ibus-bridge",
        "gtk4-layer-shell",
        "pam.d/tuna-lock",
        "tuna-greeter",
        "tuna.desktop",
        "check()",
        "--locked",
    ] {
        assert!(pkgbuild.contains(token), "PKGBUILD keeps {token}");
    }
    let containerfile = read_workspace("packaging/marlin/Containerfile");
    assert!(containerfile.contains("ghcr.io/tuna-os/marlin:gnome"));
    // Selectable at login now that the lock screen unlocks through PAM
    // under GDM (#62): the session and its PAM service must both ship.
    assert!(containerfile.contains("Name=Tuna Desktop (preview)"));
    assert!(containerfile.contains("test -f /etc/pam.d/tuna-lock"));
    assert!(!containerfile.contains("mv /usr/share/wayland-sessions/tuna.desktop"));
    // Both installed executables are checked through the bounded diagnostic
    // loop; a missing executable or failed version command aborts the build.
    assert!(containerfile.contains("for bin in tuna-compositor tuna-shell-gtk; do"));
    assert!(containerfile.contains("command -v \"$bin\" && test -f \"/usr/bin/$bin\""));
    assert!(containerfile.contains("test -x \"/usr/bin/$bin\""));
    assert!(containerfile.contains("\"$bin\" --version || exit 1;"));
}
