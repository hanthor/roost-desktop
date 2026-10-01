//! T3 live shell overview and activation wiring (client round trip).
//!
//! [`ControlClient`] against a real [`ControlHub`] over a bound socket:
//! hello plus snapshot into the overview model, `activate_selected`
//! through the live token store with request-id correlation, and a
//! server-side revision gap that resnapshots whole state instead of
//! partial deltas. The hub is driven with bounded poll loops and the
//! client surfaces `WouldBlock` between frames, so nothing here sleeps.
//! Scripted-peer unit cases (gap flag, id correlation) live in
//! `src/control.rs`; panel surfaces, supervision, and overlay are other
//! tasks' scope. All code here is original.

use std::io;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use roost_compositor::control::ControlHub;
use roost_compositor::state::{TokenStore, MAX_CHANGE_LOG};
use roost_compositor::windows::WindowManager;
use roost_compositor::{TestCompositor, SEAT_NAME};
use roost_shell_control::CommandStatus;
use roost_shell_host::control::{ControlClient, ControlError, Handled};

const ROUNDS: usize = 2000;

/// Live harness: headless compositor plus window manager plus control
/// hub on a unique socket path, with two windows mapped in the model.
struct Harness {
    /// Owner-only dir holding the control socket; kept alive for the run.
    _dir: tempfile::TempDir,
    comp: TestCompositor,
    manager: WindowManager,
    hub: ControlHub,
    socket_path: std::path::PathBuf,
    id_a: u64,
    id_b: u64,
}

fn harness(name: &str) -> Harness {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let id_a = manager
        .model_mut()
        .insert("alpha", Some("com.example.alpha"), 0);
    let id_b = manager
        .model_mut()
        .insert("beta", Some("com.example.beta"), 0);
    assert!(manager.model_mut().set_focused(Some(id_b)));

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = private_tempdir();
    let socket_path = dir.path().join(format!(
        "roost-live-{}-{}-{nanos}.sock",
        std::process::id(),
        name
    ));
    let hub = ControlHub::bind(socket_path.clone(), Rc::new(TokenStore::new()), SEAT_NAME).unwrap();
    Harness {
        _dir: dir,
        comp,
        manager,
        hub,
        socket_path,
        id_a,
        id_b,
    }
}

/// Drive the hub until `client.poll()` yields something other than
/// `WouldBlock`, returning that outcome. Every iteration pumps both
/// sides, so buffered frames converge without sleeping.
fn drive(h: &mut Harness, client: &mut ControlClient) -> Handled {
    for _ in 0..ROUNDS {
        h.hub.poll(h.manager.model_mut());
        h.comp.pump();
        match client.poll() {
            Err(ControlError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => panic!("client poll failed: {e:?}"),
            Ok(handled) => return handled,
        }
    }
    panic!("drive budget exhausted");
}

/// Like [`drive`], but skips compositor overview intents: `Overview`
/// frames are UI state outside the ordered model stream, so tests
/// awaiting model frames (deltas, results, resnapshots) step past them.
fn drive_model(h: &mut Harness, client: &mut ControlClient) -> Handled {
    for _ in 0..ROUNDS {
        let handled = drive(h, client);
        if !matches!(handled, Handled::Overview { .. }) {
            return handled;
        }
    }
    panic!("drive budget exhausted");
}

/// Drive the hub until `client.await_hello()` succeeds.
fn drive_hello(h: &mut Harness, client: &mut ControlClient) {
    for _ in 0..ROUNDS {
        h.hub.poll(h.manager.model_mut());
        h.comp.pump();
        match client.await_hello() {
            Err(ControlError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => panic!("hello failed: {e:?}"),
            Ok(()) => return,
        }
    }
    panic!("hello budget exhausted");
}

/// Connected client holding the handshake snapshot.
fn live_client(h: &mut Harness) -> ControlClient {
    let mut client = ControlClient::connect(&h.socket_path).unwrap();
    assert!(client.needs_snapshot());
    client.send_hello().unwrap();
    drive_hello(h, &mut client);
    match drive(h, &mut client) {
        Handled::Snapshot { .. } => {}
        other => panic!("expected initial Snapshot, got {other:?}"),
    }
    // Newcomers join mid-state: the hub states the current intent next.
    match drive(h, &mut client) {
        Handled::Overview { open } => assert!(!open, "fresh hub starts closed"),
        other => panic!("expected newcomer Overview, got {other:?}"),
    }
    client
}

#[test]
fn live_snapshot_lists_windows_and_workspace_state() {
    let mut h = harness("snapshot");
    let client = live_client(&mut h);

    assert!(!client.needs_snapshot());
    assert_eq!(client.revision(), Some(h.manager.model().revision()));
    let model = client.model();
    assert_eq!(model.windows().len(), 2);
    let titles: Vec<&str> = model.windows().iter().map(|w| w.title.as_str()).collect();
    assert!(titles.contains(&"alpha") && titles.contains(&"beta"));
    assert_eq!(model.workspaces(), &[0]);
    // Snapshot focus maps to the overview selection.
    assert_eq!(model.selected(), Some(h.id_b));
    assert!(!model.is_overview_open(), "snapshot keeps UI flag");
}

#[test]
fn activate_selected_drives_token_gated_focus() {
    let mut h = harness("activate");
    let mut client = live_client(&mut h);
    assert_eq!(client.model().selected(), Some(h.id_b));

    // Move compositor focus out from under the shell without telling it:
    // the shell still selects B with B's snapshot token, so activating
    // must move focus back through token validation.
    assert!(h.manager.model_mut().set_focused(Some(h.id_a)));
    let request = client.activate_selected().unwrap();

    // First the hub's focus-A delta arrives, then the CommandResult.
    match drive_model(&mut h, &mut client) {
        Handled::Changes { .. } => {}
        other => panic!("expected focus delta first, got {other:?}"),
    }
    match drive_model(&mut h, &mut client) {
        Handled::CommandResult { id, status } => {
            assert_eq!(id, request, "result echoes the request id");
            assert_eq!(status, CommandStatus::Applied);
        }
        other => panic!("expected CommandResult, got {other:?}"),
    }
    assert_eq!(h.manager.model().focused(), Some(h.id_b));

    // The activation's own focus delta follows and moves the overview
    // selection back to B.
    match drive_model(&mut h, &mut client) {
        Handled::Changes { .. } => {}
        other => panic!("expected trailing focus delta, got {other:?}"),
    }
    assert_eq!(client.model().selected(), Some(h.id_b));
}

#[test]
fn revision_gap_overflow_resnapshots_whole_state() {
    let mut h = harness("gap");
    let mut client = live_client(&mut h);
    let base = client.revision().unwrap();

    // Advance the model past the change log without driving the hub, so
    // the session cursor is unrecoverable and the hub must resnapshot.
    for i in 0..(MAX_CHANGE_LOG + 10) {
        h.manager
            .model_mut()
            .insert(&format!("w{i}"), Some("com.example.flood"), 0);
    }
    let current = h.manager.model().revision();
    assert!(current > base + MAX_CHANGE_LOG as u64);

    // The first frame after the gap is a full Snapshot, never a partial
    // Changes onto the stale base.
    match drive_model(&mut h, &mut client) {
        Handled::Snapshot { revision } => assert_eq!(revision, current),
        other => panic!("gap must resnapshot whole state, got {other:?}"),
    }
    assert!(!client.needs_snapshot());
    assert_eq!(client.revision(), Some(current));
    let model = client.model();
    assert_eq!(model.windows().len(), 2 + MAX_CHANGE_LOG + 10);
    assert!(
        model.windows().iter().any(|w| w.title == "alpha")
            && model.windows().iter().any(|w| w.title == "w0"),
        "resnapshot carries old and new windows together"
    );
}

/// Temp dir with owner-only permissions: the control socket refuses to
/// bind anywhere less private (#30).
fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
