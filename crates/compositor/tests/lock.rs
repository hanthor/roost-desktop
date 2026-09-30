//! Session-lock flag and surface tests (session-lock flag task).
//!
//! Covers both acceptance criteria at the hub/session level. The live
//! `Runtime` needs a GPU backend, so the idle machine itself is
//! unit-tested in `lock.rs` and mirrored into the hub here exactly as
//! the runtime does. First, idle timeout produces the lock state and
//! hides all windows: the hub snapshot flips to `locked` with an empty
//! window list. Second, shell restart (reconnect) while locked keeps
//! the lock screen up: a fresh handshake receives the locked,
//! content-free snapshot. Also covered: the control-command set path
//! (`Lock`), delta suppression while locked, content restore on
//! unlock, and the title-free lock surface.
//!
//! All code here is original.

use std::cell::Cell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use roost_compositor::control::{
    deny_all_tokens, ControlConn, ControlHub, Emitted, Handled, Session,
};
use roost_compositor::lock::{content_visible, SessionLock};
use roost_compositor::overlay::Overlay;
use roost_compositor::state::{StateModel, TokenStore};
use roost_compositor::SEAT_NAME;
use roost_shell_control::{
    decode_frame, encode_frame, CommandKind, CommandStatus, Message, CURRENT_VERSION,
};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Blocking raw client plus framed read/write helpers (control.rs style).
fn client_write(client: &mut UnixStream, msg: &Message) {
    client.write_all(&encode_frame(msg)).unwrap();
    client.flush().unwrap();
}

fn client_read(client: &mut UnixStream) -> Message {
    let mut prefix = [0u8; 4];
    client.read_exact(&mut prefix).unwrap();
    let len = u32::from_le_bytes(prefix) as usize;
    let mut body = vec![0u8; len];
    client.read_exact(&mut body).unwrap();
    let mut frame = prefix.to_vec();
    frame.extend_from_slice(&body);
    decode_frame(&frame).unwrap()
}

fn hello() -> Message {
    Message::Hello {
        version: CURRENT_VERSION,
    }
}

fn connect(path: &std::path::Path) -> UnixStream {
    let client = UnixStream::connect(path).unwrap();
    client
        .set_read_timeout(Some(TIMEOUT))
        .and(client.set_write_timeout(Some(TIMEOUT)))
        .unwrap();
    client
}

/// Model with two titled windows on workspace 1.
fn two_window_model() -> StateModel {
    let mut model = StateModel::new();
    model.insert("Terminal", Some("org.example.Terminal"), 1);
    model.insert("Browser", Some("org.example.Browser"), 1);
    model
}

fn assert_unlocked_with_titles(snapshot: &Message) {
    let Message::Snapshot {
        windows, locked, ..
    } = snapshot
    else {
        panic!("expected Snapshot, got {snapshot:?}");
    };
    assert!(!locked, "fresh session starts unlocked");
    assert_eq!(windows.len(), 2, "unlocked snapshot carries windows");
    assert!(
        windows.iter().any(|w| w.title == "Terminal"),
        "unlocked snapshot carries titles"
    );
}

fn assert_locked_without_content(snapshot: &Message) {
    let Message::Snapshot {
        windows, locked, ..
    } = snapshot
    else {
        panic!("expected Snapshot, got {snapshot:?}");
    };
    assert!(locked, "locked session reports locked");
    assert!(
        windows.is_empty(),
        "locked snapshot carries no window content, got {} windows",
        windows.len()
    );
}

/// Idle timeout shows the lock state and hides all windows: the idle
/// machine trips (as the runtime evaluates it from input timestamps),
/// the runtime mirrors it into the hub, and the next poll strips the
/// snapshot.
#[test]
fn idle_timeout_strips_snapshot_windows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let mut hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT_NAME).unwrap();
    let mut model = two_window_model();

    let mut client = connect(&path);
    hub.poll(&mut model);
    client_write(&mut client, &hello());
    hub.poll(&mut model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert_unlocked_with_titles(&client_read(&mut client));
    // Newcomers join mid-state: the handshake also carries the current
    // overview intent. Drain it so later reads see lock traffic only.
    assert!(matches!(
        client_read(&mut client),
        Message::Overview { open: false }
    ));

    // Idle machine from input timestamps: activity at t=1000, timeout
    // 60 s, still clear at 30 s, tripped at 61 s.
    let mut idle = SessionLock::new(60_000);
    idle.note_input(1_000);
    hub.poll(&mut model);
    assert!(!idle.check_timeout(31_000));
    assert!(!hub.is_locked());
    assert!(idle.check_timeout(61_000));
    // Runtime mirrors the trip into the hub exactly as engage_lock does.
    hub.set_locked(true);
    assert!(hub.is_locked());

    hub.poll(&mut model);
    assert_locked_without_content(&client_read(&mut client));
}

/// Shell restart (reconnect) while locked keeps the lock screen up: a
/// fresh handshake receives the locked, content-free snapshot.
#[test]
fn reconnect_while_locked_stays_locked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let mut hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT_NAME).unwrap();
    let mut model = two_window_model();

    let mut first = connect(&path);
    hub.poll(&mut model);
    client_write(&mut first, &hello());
    hub.poll(&mut model);
    assert!(matches!(client_read(&mut first), Message::Hello { .. }));
    assert_unlocked_with_titles(&client_read(&mut first));
    // Drain the handshake overview intent (see the idle test above).
    assert!(matches!(
        client_read(&mut first),
        Message::Overview { open: false }
    ));

    hub.set_locked(true);
    hub.poll(&mut model);
    assert_locked_without_content(&client_read(&mut first));

    // The shell restarts: a brand-new connection handshakes mid-lock.
    let mut second = connect(&path);
    hub.poll(&mut model);
    client_write(&mut second, &hello());
    hub.poll(&mut model);
    assert!(matches!(client_read(&mut second), Message::Hello { .. }));
    assert_locked_without_content(&client_read(&mut second));

    // And the first session still sees the lock too.
    hub.poll(&mut model);
    assert_eq!(
        hub.session_count(),
        2,
        "both shell generations stay connected"
    );
}

/// Control-command set path: `Lock` applies at once, the next poll
/// strips the snapshot, deltas stay suppressed while locked, and
/// clearing the flag restores full content.
#[test]
fn lock_command_engages_and_unlock_restores() {
    let mut model = two_window_model();
    let (server, mut client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(TIMEOUT))
        .and(client.set_write_timeout(Some(TIMEOUT)))
        .unwrap();
    let conn = ControlConn::new(server).unwrap();
    let locked = Rc::new(Cell::new(false));
    client_write(&mut client, &hello());
    let mut session = Session::handshake_with(
        conn,
        &model,
        deny_all_tokens,
        Rc::new(|_| String::new()),
        Rc::new(Cell::new(false)),
        locked.clone(),
    )
    .unwrap();
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert_unlocked_with_titles(&client_read(&mut client));

    client_write(
        &mut client,
        &Message::Command {
            id: 1,
            kind: CommandKind::Lock,
        },
    );
    let handled = session.handle_next(&mut model).unwrap();
    assert!(
        matches!(
            handled,
            Handled::CommandResult {
                id: 1,
                applied: true
            }
        ),
        "Lock applies at once, got {handled:?}"
    );
    let Message::CommandResult { id, status } = client_read(&mut client) else {
        panic!("expected CommandResult for Lock");
    };
    assert_eq!(id, 1);
    assert_eq!(status, CommandStatus::Applied);
    assert!(locked.get(), "Lock flips the compositor-owned flag");

    // Next poll carries the stripped snapshot (lock transition forces
    // a resnapshot even with no model change).
    let emitted = session.emit_deltas(&model).unwrap();
    assert!(matches!(emitted, Emitted::Snapshot { .. }));
    assert_locked_without_content(&client_read(&mut client));

    // Window changes while locked never reach the shell: cursor
    // advances, nothing is sent.
    model.insert("Secrets", Some("org.example.Secrets"), 1);
    let emitted = session.emit_deltas(&model).unwrap();
    assert!(matches!(emitted, Emitted::Idle { .. }));
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut probe = [0u8; 1];
    let err = client.read(&mut probe).unwrap_err();
    assert!(
        err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::TimedOut,
        "no delta may leak while locked, got {err:?}"
    );
    client.set_read_timeout(Some(TIMEOUT)).unwrap();

    // Unlock (session auth, stubbed here at the flag) restores the
    // full snapshot including the window opened while locked.
    locked.set(false);
    let emitted = session.emit_deltas(&model).unwrap();
    assert!(matches!(emitted, Emitted::Snapshot { .. }));
    let Message::Snapshot {
        windows,
        locked,
        revision,
        ..
    } = client_read(&mut client)
    else {
        panic!("expected restoring Snapshot after unlock");
    };
    assert!(!locked);
    assert_eq!(revision, model.revision());
    assert_eq!(windows.len(), 3, "unlock restores every window");
    assert!(
        windows.iter().any(|w| w.title == "Secrets"),
        "window opened while locked appears after unlock"
    );
}

/// The lock surface reuses the Overlay shape with an empty list: no
/// window content or titles, and no frame content beneath it.
#[test]
fn lock_surface_carries_no_titles() {
    // Exactly what the runtime serves while locked: visible, empty.
    let mut overlay = Overlay::new(3);
    overlay.show(Vec::new());
    assert!(overlay.visible);
    assert!(overlay.windows.is_empty());
    assert_eq!(overlay.selected, None);

    assert!(!content_visible(true), "locked frames hide content");
    assert!(content_visible(false), "unlocked frames show content");
}

/// Three wrong passwords stay locked with no content leaked: each
/// denied unlock attempt leaves the flag, the surface, and the hub
/// mirror exactly as they were.
#[test]
fn three_wrong_passwords_stay_locked_without_leak() {
    use greetd_ipc::{codec::Error as CodecError, AuthMessageType, ErrorType, Response};
    use roost_compositor::unlock::{unlock_session, UnlockClient};

    struct DenyAll;
    impl UnlockClient for DenyAll {
        fn create_session(&mut self, _user: &str) -> Result<Response, CodecError> {
            Ok(Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                auth_message: "Password:".to_string(),
            })
        }
        fn answer(&mut self, _text: Option<String>) -> Result<Response, CodecError> {
            Ok(Response::Error {
                error_type: ErrorType::AuthError,
                description: "PAM authentication failed".to_string(),
            })
        }
        fn cancel(&mut self) -> Result<(), CodecError> {
            Ok(())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT_NAME).unwrap();
    let mut lock = SessionLock::new(60_000);
    let mut overlay = Overlay::new(3);
    lock.lock();
    hub.set_locked(true);
    overlay.show(Vec::new());
    let mut client = DenyAll;
    for _ in 0..3 {
        assert!(
            !unlock_session(
                &mut lock,
                &hub,
                &mut overlay,
                0,
                "mallory",
                "wrong",
                &mut client
            ),
            "wrong password denies"
        );
        assert!(lock.is_locked(), "flag survives wrong password");
        assert!(overlay.visible, "surface stays up");
        assert!(overlay.windows.is_empty(), "no content leaks");
        assert!(hub.is_locked(), "hub mirror stays locked");
    }
}
