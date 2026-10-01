//! Greetd unlock path tests (session-lock unlock task).
//!
//! Both acceptance criteria, with auth stubbed at the client seam (no
//! live greetd in CI). A password-checking stub stands in for the
//! daemon behind [`UnlockClient`](roost_compositor::unlock::UnlockClient):
//! it answers `Success` only when the conversation carries the right
//! secret, exactly what the real daemon decides. First, the correct
//! password dismisses the lock and restores the session intact
//! (titles back, same revision). Second, three wrong passwords keep
//! the session locked with no window content leaked anywhere.
//!
//! All code here is original.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use greetd_ipc::{codec::Error as CodecError, AuthMessageType, ErrorType, Response};
use roost_compositor::control::ControlHub;
use roost_compositor::lock::SessionLock;
use roost_compositor::overlay::Overlay;
use roost_compositor::state::{StateModel, TokenStore};
use roost_compositor::unlock::{unlock_session, UnlockClient};
use roost_compositor::SEAT_NAME;
use roost_shell_control::{decode_frame, encode_frame, Message, CURRENT_VERSION};

const TIMEOUT: Duration = Duration::from_secs(5);
const PROBE: Duration = Duration::from_millis(200);

/// Fake daemon behind the client seam: succeeds only when the
/// conversation carries the expected secret, fails generic otherwise.
struct CheckingStub {
    expected: String,
    answers: Vec<Option<String>>,
    cancelled: bool,
}

impl CheckingStub {
    fn new(expected: &str) -> Self {
        Self {
            expected: expected.to_string(),
            answers: Vec::new(),
            cancelled: false,
        }
    }
}

impl UnlockClient for CheckingStub {
    fn create_session(&mut self, _user: &str) -> Result<Response, CodecError> {
        Ok(Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password:".to_string(),
        })
    }

    fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
        self.answers.push(text.clone());
        if text.as_deref() == Some(self.expected.as_str()) {
            Ok(Response::Success)
        } else {
            Ok(Response::Error {
                error_type: ErrorType::AuthError,
                description: "PAM authentication failed".to_string(),
            })
        }
    }

    fn cancel(&mut self) -> Result<(), CodecError> {
        self.cancelled = true;
        Ok(())
    }
}

/// Blocking raw client plus framed read/write helpers (lock.rs style).
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

/// Handshake one shell client; drain the handshake overview intent so
/// later reads see lock traffic only. Returns the unlocked snapshot.
fn handshake_shell(
    hub: &mut ControlHub,
    model: &mut StateModel,
    path: &std::path::Path,
) -> UnixStream {
    let mut client = connect(path);
    hub.poll(model);
    client_write(&mut client, &hello());
    hub.poll(model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    let snapshot = client_read(&mut client);
    assert!(matches!(
        client_read(&mut client),
        Message::Overview { open: false }
    ));
    let Message::Snapshot { locked, .. } = &snapshot else {
        panic!("expected Snapshot, got {snapshot:?}");
    };
    assert!(!locked, "fresh session starts unlocked");
    client
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

/// No bytes may be waiting: while locked, deltas are suppressed, so a
/// shell-side read must time out rather than deliver content.
fn assert_nothing_sent(client: &mut UnixStream) {
    client.set_read_timeout(Some(PROBE)).unwrap();
    let mut probe = [0u8; 1];
    let err = client.read(&mut probe).unwrap_err();
    assert!(
        err.kind() == std::io::ErrorKind::WouldBlock || err.kind() == std::io::ErrorKind::TimedOut,
        "nothing may reach the shell while locked, got {err:?}"
    );
    client.set_read_timeout(Some(TIMEOUT)).unwrap();
}

/// Correct password dismisses the lock and restores the session
/// intact: flags clear, the lock surface drops, and the next poll
/// carries every window with titles at the current revision.
#[test]
fn correct_password_dismisses_lock_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let mut hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT_NAME).unwrap();
    let mut model = two_window_model();
    let mut client = handshake_shell(&mut hub, &mut model, &path);

    // Lock exactly as the runtime does: idle machine, hub mirror, and
    // the exclusive surface with an empty list.
    let mut idle = SessionLock::new(60_000);
    idle.note_input(1_000);
    let mut overlay = Overlay::new(3);
    idle.lock();
    hub.set_locked(true);
    overlay.show(Vec::new());
    hub.poll(&mut model);
    assert_locked_without_content(&client_read(&mut client));

    // The correct password clears everything through the greeter path.
    let mut stub = CheckingStub::new("s3cret");
    assert!(unlock_session(
        &mut idle,
        &hub,
        &mut overlay,
        31_000,
        "ada",
        "s3cret",
        &mut stub
    ));
    assert_eq!(stub.answers, vec![Some("s3cret".to_string())]);
    assert!(stub.cancelled, "success releases the attempt, never starts");
    assert!(!idle.is_locked(), "idle flag clears");
    assert!(!hub.is_locked(), "hub mirror clears");
    assert!(!overlay.visible, "lock surface drops");
    // The idle accumulator restarts: no instant relock on next check.
    assert!(!idle.check_timeout(90_999));

    hub.poll(&mut model);
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
    assert_eq!(windows.len(), 2, "unlock restores every window");
    assert!(
        windows.iter().any(|w| w.title == "Terminal"),
        "titles survive the lock round-trip"
    );
    assert!(
        windows.iter().any(|w| w.title == "Browser"),
        "titles survive the lock round-trip"
    );
}

/// Three wrong passwords keep the session locked with no window
/// content leaked: flags and surface untouched, deltas suppressed, and
/// neither titles nor the entered secret appear in any shell traffic.
#[test]
fn triple_wrong_password_stays_locked_without_leak() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let mut hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT_NAME).unwrap();
    let mut model = two_window_model();
    let mut client = handshake_shell(&mut hub, &mut model, &path);

    // Every message the shell receives from here on is recorded so
    // the leak scan below covers real traffic, not expectations.
    let mut received: Vec<String> = Vec::new();

    let mut idle = SessionLock::new(60_000);
    idle.note_input(1_000);
    let mut overlay = Overlay::new(3);
    idle.lock();
    hub.set_locked(true);
    overlay.show(Vec::new());
    hub.poll(&mut model);
    let snapshot = client_read(&mut client);
    assert_locked_without_content(&snapshot);
    received.push(format!("{snapshot:?}"));

    // A window opened while locked must never reach the shell either.
    model.insert("Secrets", Some("org.example.Secrets"), 1);

    let mut stub = CheckingStub::new("s3cret");
    for attempt in 1..=3 {
        assert!(
            !unlock_session(
                &mut idle,
                &hub,
                &mut overlay,
                31_000,
                "mallory",
                "wrong",
                &mut stub
            ),
            "wrong password #{attempt} must not unlock"
        );
        assert!(idle.is_locked(), "idle flag holds after #{attempt}");
        assert!(hub.is_locked(), "hub mirror holds after #{attempt}");
        assert!(overlay.visible, "lock surface holds after #{attempt}");
        assert!(overlay.windows.is_empty(), "surface stays title-free");

        hub.poll(&mut model);
        assert_nothing_sent(&mut client);
    }
    assert!(!stub.cancelled, "failure never releases-into-start");
    assert_eq!(
        stub.answers,
        vec![
            Some("wrong".to_string()),
            Some("wrong".to_string()),
            Some("wrong".to_string())
        ],
        "every attempt carried the entered credential, nothing else"
    );

    // A shell reconnect mid-attack still gets a stripped snapshot.
    let mut second = connect(&path);
    hub.poll(&mut model);
    client_write(&mut second, &hello());
    hub.poll(&mut model);
    assert!(matches!(client_read(&mut second), Message::Hello { .. }));
    let snapshot = client_read(&mut second);
    assert_locked_without_content(&snapshot);
    received.push(format!("{snapshot:?}"));

    // No shell traffic after locking may carry window content or the
    // entered secret — titles, app ids, or the password itself.
    for msg in &received {
        for leaked in [
            "Terminal",
            "Browser",
            "Secrets",
            "org.example",
            "wrong",
            "s3cret",
        ] {
            assert!(
                !msg.contains(leaked),
                "locked traffic leaks {leaked:?}: {msg}"
            );
        }
    }
}
