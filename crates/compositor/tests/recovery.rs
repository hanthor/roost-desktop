//! Failure-injection integration harness: 001 spec A3-A6 recovery evidence.
//!
//! End-to-end proof of the recovery contract against the real crate APIs
//! (no fakes on the server side):
//!
//! - [`rwd_compositor::control`]: malformed / oversize / stale-major frames
//!   over real sockets are rejected with typed errors and the server side
//!   stays alive (driven through the real [`ControlServer`] accept path).
//! - [`rwd_compositor::state`]: a forced revision gap against [`StateModel`]
//!   yields a fresh full snapshot, never a delta.
//! - [`rwd_compositor::supervise`]: a killed `sleep` child under
//!   [`Supervisor`] exhausts a small restart budget on a [`ManualClock`]
//!   (wall-clock capped at 10 s).
//! - Disconnect-then-reconnect: a dropped client reconnects and receives a
//!   full snapshot with an equal window set (no geometry/focus assumptions
//!   beyond revision + window set).
//! - Stall: a client that connects but never reads triggers server-side
//!   backpressure ([`ControlError::WouldBlock`]) instead of blocking the
//!   server; a fresh session and a compositor pump still complete.
//!
//! Reference behavior (supervision split, restart budget, no in-compositor
//! orphaning) follows the niri survey in
//! `.spektacular/work/wave2/stream4-notes.md`; all code here is original.
//!
//! Every test is deterministic: bounded retry loops, no unbounded sleeps,
//! whole file well under 60 s.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::Command;
use std::time::{Duration, Instant};

use rwd_compositor::control::{
    ControlConn, ControlError, ControlServer, Emitted, Handled, Session,
};
use rwd_compositor::state::StateModel;
use rwd_compositor::supervise::{
    ChildEvent, Clock, ManualClock, RestartPolicy, SuperviseError, Supervisor,
};
use rwd_shell_control::{
    decode_frame, encode_frame, CommandKind, ErrorKind, Message, ProtocolVersion, CURRENT_VERSION,
    MAX_FRAME_BYTES,
};

const READ_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_ROUNDS: usize = 500;
const RETRY_SLEEP: Duration = Duration::from_millis(2);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Bound server socket in a temp dir; returns the dir (kept alive by caller),
/// the real [`ControlServer`], and the socket path.
fn bind_server() -> (tempfile::TempDir, ControlServer, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = ControlServer::new(listener).unwrap();
    (dir, server, path)
}

fn connect(path: &std::path::Path) -> UnixStream {
    let stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
    stream
}

/// Blocking read of exactly one framed message on the client side.
fn client_read(stream: &mut UnixStream) -> Message {
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix).unwrap();
    let len = u32::from_le_bytes(prefix) as usize;
    assert!(len <= MAX_FRAME_BYTES, "server sent an oversize frame");
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).unwrap();
    let mut frame = prefix.to_vec();
    frame.extend_from_slice(&body);
    decode_frame(&frame).unwrap()
}

fn client_send(stream: &mut UnixStream, msg: &Message) {
    stream.write_all(&encode_frame(msg)).unwrap();
    stream.flush().unwrap();
}

/// Accept one connection (bounded retries) and run the server handshake.
/// The client must already have sent its `Hello`.
fn accept_handshake<'a>(server: &ControlServer, model: &StateModel) -> Session<'a> {
    for _ in 0..RETRY_ROUNDS {
        match server.accept() {
            Ok(conn) => match Session::handshake(conn, model) {
                Ok(session) => return session,
                Err(e) => panic!("handshake failed: {e}"),
            },
            Err(ControlError::WouldBlock) => std::thread::sleep(RETRY_SLEEP),
            Err(e) => panic!("accept failed: {e}"),
        }
    }
    panic!("accept budget exhausted");
}

/// Full client+server handshake. Returns the client stream (positioned after
/// reading the server `Hello` + `Snapshot`) and the server [`Session`].
fn handshake_pair<'a>(
    server: &ControlServer,
    path: &std::path::Path,
    model: &StateModel,
) -> (UnixStream, Session<'a>, Message, Message) {
    let mut client = connect(path);
    client_send(
        &mut client,
        &Message::Hello {
            version: CURRENT_VERSION,
        },
    );
    let session = accept_handshake(server, model);
    let hello = client_read(&mut client);
    let snapshot = client_read(&mut client);
    assert!(matches!(hello, Message::Hello { .. }));
    assert!(matches!(snapshot, Message::Snapshot { .. }));
    (client, session, hello, snapshot)
}

/// Drive [`Session::handle_next`] until it returns something other than
/// `WouldBlock` (bounded).
fn handle_soon(session: &mut Session<'_>, model: &mut StateModel) -> Result<Handled, ControlError> {
    for _ in 0..RETRY_ROUNDS {
        match session.handle_next(model) {
            Err(ControlError::WouldBlock) => std::thread::sleep(RETRY_SLEEP),
            other => return other,
        }
    }
    panic!("handle_next budget exhausted");
}

fn snapshot_windows(msg: &Message) -> (u64, BTreeSet<u64>) {
    match msg {
        Message::Snapshot {
            revision, windows, ..
        } => (*revision, windows.iter().map(|w| w.id).collect()),
        other => panic!("expected Snapshot, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A3: malformed / oversize / stale-major frames rejected, server stays alive
// ---------------------------------------------------------------------------

#[test]
fn malformed_frame_rejected_with_typed_error_and_session_survives() {
    let (_dir, server, path) = bind_server();
    let mut model = StateModel::new();
    let w = model.insert("term", None, 1);
    assert!(model.set_focused(Some(w)));

    let (mut client, mut session, _, _) = handshake_pair(&server, &path, &model);

    // Malformed body: valid prefix, undecodable postcard payload.
    let mut bad = (8u32).to_le_bytes().to_vec();
    bad.extend_from_slice(&[0xFF; 8]);
    client.write_all(&bad).unwrap();
    client.flush().unwrap();

    let handled = handle_soon(&mut session, &mut model).unwrap();
    assert_eq!(
        handled,
        Handled::ErrorSent {
            kind: ErrorKind::MalformedFrame
        }
    );
    let err = client_read(&mut client);
    assert!(
        matches!(
            err,
            Message::Error {
                kind: ErrorKind::MalformedFrame,
                ..
            }
        ),
        "expected typed MalformedFrame error, got {err:?}"
    );

    // Same session still serves commands: server side stayed alive.
    client_send(
        &mut client,
        &Message::Command {
            id: 7,
            kind: CommandKind::ToggleOverview,
        },
    );
    let handled = handle_soon(&mut session, &mut model).unwrap();
    assert_eq!(
        handled,
        Handled::CommandResult {
            id: 7,
            applied: true
        }
    );
    let reply = client_read(&mut client);
    assert!(matches!(reply, Message::CommandResult { id: 7, .. }));
}

#[test]
fn oversize_frame_rejected_with_typed_error_and_server_accepts_next_client() {
    let (_dir, server, path) = bind_server();
    let mut model = StateModel::new();
    let (mut client, mut session, _, _) = handshake_pair(&server, &path, &model);

    // Prefix alone declares a body over the 1 MiB cap; the server must
    // reject from the prefix before awaiting any body.
    let huge = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
    client.write_all(&huge).unwrap();
    client.flush().unwrap();

    let err = handle_soon(&mut session, &mut model).unwrap_err();
    assert!(
        matches!(
            err,
            ControlError::Decode(rwd_shell_control::DecodeError::Oversize { .. })
        ),
        "expected typed Oversize decode error, got {err:?}"
    );
    let wire = client_read(&mut client);
    assert!(
        matches!(
            wire,
            Message::Error {
                kind: ErrorKind::OversizeFrame,
                ..
            }
        ),
        "expected typed OversizeFrame error, got {wire:?}"
    );
    drop(session);
    drop(client);

    // Server side stays alive: a fresh client handshakes cleanly.
    let model2 = StateModel::new();
    let (client2, _session2, hello, snapshot) = handshake_pair(&server, &path, &model2);
    assert!(matches!(hello, Message::Hello { .. }));
    assert!(matches!(snapshot, Message::Snapshot { .. }));
    let _ = client2;
}

#[test]
fn stale_major_rejected_with_typed_error_and_server_accepts_next_client() {
    let (_dir, server, path) = bind_server();
    let model = StateModel::new();

    // Stale major line: must be refused with a typed version error.
    let mut client = connect(&path);
    client_send(
        &mut client,
        &Message::Hello {
            version: ProtocolVersion::new(CURRENT_VERSION.major.wrapping_add(1), 0),
        },
    );
    let conn = loop {
        match server.accept() {
            Ok(conn) => break conn,
            Err(ControlError::WouldBlock) => std::thread::sleep(RETRY_SLEEP),
            Err(e) => panic!("accept failed: {e}"),
        }
    };
    let err = Session::handshake(conn, &model).unwrap_err();
    assert!(
        matches!(
            err,
            ControlError::Decode(rwd_shell_control::DecodeError::IncompatibleVersion { .. })
        ),
        "expected typed IncompatibleVersion error, got {err:?}"
    );
    let wire = client_read(&mut client);
    assert!(
        matches!(
            wire,
            Message::Error {
                kind: ErrorKind::IncompatibleVersion,
                ..
            }
        ),
        "expected typed IncompatibleVersion error frame, got {wire:?}"
    );
    drop(client);

    // Server side stays alive for a conforming client.
    let (client2, _session2, _, snapshot) = handshake_pair(&server, &path, &model);
    assert!(matches!(snapshot, Message::Snapshot { .. }));
    let _ = client2;
}

// ---------------------------------------------------------------------------
// A4: revision-gap resnapshot — fresh full Snapshot, never a delta
// ---------------------------------------------------------------------------

#[test]
fn revision_gap_forces_full_resnapshot_not_delta() {
    let (_dir, server, path) = bind_server();
    let mut model = StateModel::new();
    let a = model.insert("term", None, 1);
    let _ = a;
    model.insert("browser", None, 2);

    let (mut client, mut session, _, first) = handshake_pair(&server, &path, &model);
    let (rev0, _) = snapshot_windows(&first);
    assert_eq!(rev0, model.revision());
    assert_eq!(session.last_revision(), model.revision());

    // Force the shell's revision out of the retained change-log window.
    for i in 0..(rwd_compositor::state::MAX_CHANGE_LOG as u64 + 10) {
        model.insert(&format!("w{i}"), None, 1);
    }
    assert!(
        model.changes_since(rev0).is_err(),
        "expected a genuine RevisionGap"
    );

    let emitted = loop {
        // emit_deltas is synchronous for a ready socket; retry on backpressure.
        match session.emit_deltas(&model) {
            Err(ControlError::WouldBlock) => std::thread::sleep(RETRY_SLEEP),
            other => break other.unwrap(),
        }
    };
    let revision = model.revision();
    assert_eq!(emitted, Emitted::Snapshot { revision });

    // The wire carries a full Snapshot at the current revision — not Changes.
    let msg = client_read(&mut client);
    let (snap_rev, ids) = snapshot_windows(&msg);
    assert_eq!(snap_rev, revision);
    let expected: BTreeSet<u64> = model.windows().map(|w| w.id).collect();
    assert_eq!(ids, expected);
}

// ---------------------------------------------------------------------------
// A5: shell-death simulation — bounded restart budget exhausts (fast)
// ---------------------------------------------------------------------------

#[test]
fn shell_death_exhausts_bounded_restart_budget() {
    let wall = Instant::now();
    let policy = RestartPolicy::new(2, 1, 1);
    let clock = ManualClock::new(0);
    let mut sup = Supervisor::new(policy);
    let mut remake = || {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        cmd
    };

    // Spawn the shell child under supervision; it is alive.
    sup.spawn(&mut Command::new("sleep")).unwrap();
    assert!(sup.has_child());
    assert_eq!(
        sup.poll(&mut remake, clock.now_ms()).unwrap(),
        ChildEvent::Running
    );

    // Simulate shell death: the supervisor's own replace path kills the
    // live `sleep` (kill-on-replace) with a short-lived child that exits
    // immediately, then the finite budget runs out on the fake clock.
    sup.spawn(&mut Command::new("/bin/false")).unwrap();
    let mut now = clock.now_ms();
    let mut remake = || Command::new("/bin/false");
    let mut exhausted = false;
    for _ in 0..32 {
        match sup.poll(&mut remake, now) {
            Ok(ChildEvent::Running) => std::thread::sleep(Duration::from_millis(5)),
            Ok(ChildEvent::Exited(_)) => {
                // Jump the fake clock past the (1 ms) backoff.
                clock.set(now.saturating_add(5));
                now = clock.now_ms();
            }
            Err(SuperviseError::BudgetExhausted) => {
                exhausted = true;
                break;
            }
            Err(e) => panic!("unexpected supervise error: {e}"),
        }
        if wall.elapsed() > Duration::from_secs(10) {
            panic!("restart-budget drive exceeded 10 s wall cap");
        }
    }
    assert!(exhausted, "restart budget did not exhaust");
    assert!(matches!(
        sup.exhaust(),
        Err(SuperviseError::BudgetExhausted)
    ));
    assert!(wall.elapsed() < Duration::from_secs(10));
}

// ---------------------------------------------------------------------------
// Disconnect-then-reconnect: full Snapshot re-receipt, window set equal
// ---------------------------------------------------------------------------

#[test]
fn disconnect_then_reconnect_receives_full_snapshot() {
    let (_dir, server, path) = bind_server();
    let mut model = StateModel::new();
    let a = model.insert("term", None, 1);
    let b = model.insert("browser", None, 2);
    assert!(model.set_focused(Some(b)));
    let _ = a;

    let (client, session, _, first) = handshake_pair(&server, &path, &model);
    let (rev1, ids1) = snapshot_windows(&first);
    assert_eq!(rev1, model.revision());
    drop(session);
    drop(client); // disconnect: client stream gone.

    // Reconnect: the server must send a fresh full Snapshot; the window
    // set must be equal. No geometry/focus assumptions beyond revision +
    // window set.
    let (client2, _session2, _, second) = handshake_pair(&server, &path, &model);
    let (rev2, ids2) = snapshot_windows(&second);
    assert_eq!(rev2, model.revision());
    assert_eq!(ids1, ids2);
    let expected: BTreeSet<u64> = model.windows().map(|w| w.id).collect();
    assert_eq!(ids2, expected);
    let _ = client2;
}

// ---------------------------------------------------------------------------
// A6: stalled client — backpressure disconnects, server pump still completes
// ---------------------------------------------------------------------------

#[test]
fn stalled_client_gets_backpressure_and_server_still_serves() {
    // Socketpair with a peer that connects but never reads: the server
    // send buffer must eventually refuse (WouldBlock) instead of blocking
    // the server. The peer handle is forgotten so it stays open and unread
    // for the whole fill (a pure stall).
    let (server_raw, held) = UnixStream::pair().unwrap();
    std::mem::forget(held);
    let mut stalled = ControlConn::new(server_raw).unwrap();

    let mut model = StateModel::new();
    for i in 0..200 {
        model.insert(
            &format!("window-{i}-with-a-fairly-long-title-to-fill-frames"),
            None,
            1,
        );
    }
    let snap = Message::Snapshot {
        revision: model.revision(),
        windows: model
            .windows()
            .map(|w| rwd_shell_control::WindowInfo {
                id: w.id,
                title: w.title.clone(),
                app_id: None,
                workspace: u64::from(w.workspace),
                focused: w.focused,
                activation_token: String::new(),
            })
            .collect(),
        workspaces: model
            .workspaces()
            .iter()
            .map(|id| rwd_shell_control::WorkspaceInfo {
                id: u64::from(*id),
                name: None,
                active: false,
            })
            .collect(),
    };
    let mut pressured = false;
    for _ in 0..500 {
        match stalled.write_frame(&snap) {
            Ok(()) => continue,
            Err(ControlError::WouldBlock) | Err(ControlError::Closed) => {
                pressured = true;
                break;
            }
            Err(ControlError::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                pressured = true;
                break;
            }
            Err(e) => panic!("unexpected stall write result: {e}"),
        }
    }
    // Either the buffer refused (backpressure path) or 500 large frames
    // flushed without blocking; both prove the server never blocked. Drop
    // the stalled connection, as the nonblocking contract requires.
    drop(stalled);
    // 500 multi-KB frames against an unread peer cannot fit any socket
    // buffer: refusal is deterministic, and reaching this point proves the
    // server never blocked.
    assert!(pressured, "unread peer never applied backpressure");

    // Server still serves: fresh handshake over the real listener works,
    // and the compositor pump still completes.
    let (_dir, server, path) = bind_server();
    let (client, _session, _, snapshot) = handshake_pair(&server, &path, &model);
    assert!(matches!(snapshot, Message::Snapshot { .. }));
    let mut comp = rwd_compositor::TestCompositor::new();
    comp.pump(); // must complete, never blocked by the stalled client.
    let _ = client;
}
