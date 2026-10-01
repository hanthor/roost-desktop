//! Shipped-artifact tests: the installable session file parses through
//! the same greeter machinery that reads live system directories.

use std::path::Path;

use roost_greeter::session::enumerate_dirs;

fn share_sessions() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("share")
        .join("wayland-sessions")
}

#[test]
fn shipped_roost_desktop_parses_to_session_launcher() {
    let dir = share_sessions();
    assert!(dir.is_dir(), "missing {}", dir.display());
    let found = enumerate_dirs(std::slice::from_ref(&dir.as_path()));
    assert_eq!(found.skipped, 0, "shipped session file must parse");
    assert_eq!(found.entries.len(), 1);
    let entry = found
        .entries
        .iter()
        .find(|entry| entry.name == "Roost")
        .expect("shipped roost.desktop must enumerate as Roost");
    assert_eq!(
        entry.command,
        vec!["roost-session".to_string()],
        "greeter must launch the session binary, not the compositor directly"
    );
}
