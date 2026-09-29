//! Control-channel tests over real Unix sockets (001 R4/R6, ADR 0002).

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use roost_compositor::control::{
    ControlConn, ControlError, ControlServer, Emitted, Handled, Session,
};
use roost_compositor::state::{system_millis, StateModel, TokenStore, MAX_CHANGE_LOG};
use roost_shell_control::{
    decode_frame, encode_frame, ActivationToken, CommandKind, CommandStatus, DecodeError,
    ErrorKind, Message, ProtocolVersion, StateOp, CURRENT_VERSION,
};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Server end (nonblocking [`ControlConn`]) plus a blocking client stream.
fn pair() -> (ControlConn, UnixStream) {
    let (server, client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(TIMEOUT))
        .and(client.set_write_timeout(Some(TIMEOUT)))
        .unwrap();
    let conn = ControlConn::new(server).unwrap();
    (conn, client)
}

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

fn hello_current() -> Message {
    Message::Hello {
        version: CURRENT_VERSION,
    }
}

/// Drive a full handshake; returns the session and the model's revision.
fn handshake(conn: ControlConn, client: &mut UnixStream, model: &StateModel) -> Session<'static> {
    client_write(client, &hello_current());
    Session::handshake(conn, model).unwrap()
}

#[test]
fn hello_accept_replies_version_and_snapshot() {
    let model = StateModel::new();
    let (conn, mut client) = pair();
    let session = handshake(conn, &mut client, &model);

    let Message::Hello { version } = client_read(&mut client) else {
        panic!("expected Hello reply");
    };
    assert_eq!(version, CURRENT_VERSION);
    let Message::Snapshot {
        revision,
        windows,
        workspaces,
    } = client_read(&mut client)
    else {
        panic!("expected Snapshot after Hello");
    };
    assert_eq!(revision, 0);
    assert!(windows.is_empty());
    // Workspace 0 is always registered and active on an empty model.
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0].id, 0);
    assert!(workspaces[0].active);
    assert_eq!(session.last_revision(), 0);
}

#[test]
fn control_server_accepts_over_a_bound_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = ControlServer::new(listener).unwrap();

    let mut client = UnixStream::connect(&path).unwrap();
    client
        .set_read_timeout(Some(TIMEOUT))
        .and(client.set_write_timeout(Some(TIMEOUT)))
        .unwrap();

    let conn = server.accept().unwrap();
    let model = StateModel::new();
    let _session = handshake(conn, &mut client, &model);

    let Message::Hello { version } = client_read(&mut client) else {
        panic!("expected Hello reply via accepted conn");
    };
    assert_eq!(version, CURRENT_VERSION);
}

#[test]
fn stale_major_is_rejected_with_typed_error_and_drop() {
    let model = StateModel::new();
    let (conn, mut client) = pair();
    client_write(
        &mut client,
        &Message::Hello {
            version: ProtocolVersion::new(99, 0),
        },
    );
    let err = Session::handshake(conn, &model).unwrap_err();
    assert!(
        matches!(
            err,
            ControlError::Decode(DecodeError::IncompatibleVersion { .. })
        ),
        "unexpected: {err:?}"
    );
    // Typed error first, then the drop (handshake owns the conn, so its
    // Err path already closed the server side).
    let Message::Error { kind, .. } = client_read(&mut client) else {
        panic!("expected typed Error for stale major");
    };
    assert_eq!(kind, ErrorKind::IncompatibleVersion);
    let mut one = [0u8; 1];
    assert_eq!(client.read(&mut one).unwrap(), 0);
}

#[test]
fn snapshot_then_changes_flow() {
    let mut model = StateModel::new();
    let (conn, mut client) = pair();
    client_write(&mut client, &hello_current());
    let mut session = Session::handshake(conn, &model).unwrap();
    // Drain handshake frames: Hello + empty snapshot at rev 0.
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    let Message::Snapshot { revision, .. } = client_read(&mut client) else {
        panic!("expected initial snapshot");
    };
    assert_eq!(revision, 0);

    let id = model.insert("term", None, 1);
    let emitted = session.emit_deltas(&model).unwrap();
    assert_eq!(
        emitted,
        Emitted::Changes {
            from: 0,
            to: 1,
            ops: 1
        }
    );
    let Message::Changes {
        from_revision,
        to_revision,
        ops,
    } = client_read(&mut client)
    else {
        panic!("expected Changes after insert");
    };
    assert_eq!((from_revision, to_revision), (0, 1));
    assert_eq!(ops.len(), 1);
    assert!(
        matches!(
            &ops[0],
            StateOp::WindowOpened(w) if w.id == id && w.title == "term" && w.workspace == 1
        ),
        "unexpected ops: {ops:?}"
    );

    assert!(model.set_focused(Some(id)));
    let emitted = session.emit_deltas(&model).unwrap();
    assert_eq!(
        emitted,
        Emitted::Changes {
            from: 1,
            to: 2,
            ops: 1
        }
    );
    let Message::Changes { ops, .. } = client_read(&mut client) else {
        panic!("expected Changes after focus");
    };
    assert_eq!(ops, vec![StateOp::WindowFocused { id }]);
    assert_eq!(session.last_revision(), 2);
}

#[test]
fn unknown_message_kind_gets_typed_error() {
    let mut model = StateModel::new();
    let (conn, mut client) = pair();
    let mut session = handshake(conn, &mut client, &model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    // The shell must never send compositor-to-shell messages.
    client_write(
        &mut client,
        &Message::CommandResult {
            id: 1,
            status: CommandStatus::Applied,
        },
    );
    let handled = session.handle_next(&mut model).unwrap();
    assert_eq!(
        handled,
        Handled::ErrorSent {
            kind: ErrorKind::UnknownCommand
        }
    );
    let Message::Error { kind, message } = client_read(&mut client) else {
        panic!("expected typed Error for unknown kind");
    };
    assert_eq!(kind, ErrorKind::UnknownCommand);
    assert!(!message.is_empty());
}

#[test]
fn oversize_frame_gets_typed_error_and_drop() {
    use roost_shell_control::MAX_FRAME_BYTES;
    let mut model = StateModel::new();
    let (conn, mut client) = pair();
    let mut session = handshake(conn, &mut client, &model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    // Length prefix alone exceeds the 1 MiB cap; no body follows.
    let evil = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
    client.write_all(&evil).unwrap();
    client.flush().unwrap();

    let err = session.handle_next(&mut model).unwrap_err();
    assert!(
        matches!(err, ControlError::Decode(DecodeError::Oversize { .. })),
        "unexpected: {err:?}"
    );
    let Message::Error { kind, .. } = client_read(&mut client) else {
        panic!("expected typed Error for oversize frame");
    };
    assert_eq!(kind, ErrorKind::OversizeFrame);
    drop(session);
    let mut one = [0u8; 1];
    assert_eq!(client.read(&mut one).unwrap(), 0);
}

#[test]
fn revision_gap_triggers_resnapshot() {
    let mut model = StateModel::new();
    let (conn, mut client) = pair();
    client_write(&mut client, &hello_current());
    let mut session = Session::handshake(conn, &model).unwrap();
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    // Push the change log past its cap so revision 0 is unrecoverable.
    for i in 0..(MAX_CHANGE_LOG as u64 + 10) {
        model.insert(&format!("w{i}"), None, 1);
    }
    let current = model.revision();
    assert!(model.changes_since(0).is_err());

    let emitted = session.emit_deltas(&model).unwrap();
    assert_eq!(emitted, Emitted::Snapshot { revision: current });
    let Message::Snapshot {
        revision, windows, ..
    } = client_read(&mut client)
    else {
        panic!("expected resnapshot on revision gap");
    };
    assert_eq!(revision, current);
    assert_eq!(windows.len() as u64, current);
    assert_eq!(session.last_revision(), current);
}

#[test]
fn activation_token_denied_by_default() {
    let mut model = StateModel::new();
    let id = model.insert("term", None, 1);
    let (conn, mut client) = pair();
    let mut session = handshake(conn, &mut client, &model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    client_write(
        &mut client,
        &Message::Command {
            id: 7,
            kind: CommandKind::ActivateWindow {
                window: id,
                token: ActivationToken::new("forged".to_owned()),
            },
        },
    );
    let handled = session.handle_next(&mut model).unwrap();
    assert_eq!(
        handled,
        Handled::CommandResult {
            id: 7,
            applied: false
        }
    );
    let Message::CommandResult { id, status } = client_read(&mut client) else {
        panic!("expected CommandResult for activation");
    };
    assert_eq!(id, 7);
    let CommandStatus::Denied { reason } = status else {
        panic!("fail-closed default must deny, got {status:?}");
    };
    assert!(!reason.is_empty());
    assert_eq!(model.focused(), None);
}

#[test]
fn activation_token_live_store_allows_once_then_denies_replay() {
    let mut model = StateModel::new();
    let id = model.insert("term", None, 1);
    let store = std::rc::Rc::new(TokenStore::new());
    // Live minter/validator use the system clock, so issue fresh.
    let raw = store.issue("activate", "seat0", None, system_millis());

    // The session validates through the live store (one-use, seat-bound).
    let (conn, mut client) = pair();
    client_write(&mut client, &hello_current());
    let validate = store.clone().validator("seat0".to_owned());
    let validator = move |token: &ActivationToken, app_id: Option<&str>| validate(&token.0, app_id);
    let minter = std::rc::Rc::new(store.clone().minter("seat0".to_owned()));
    let mut session = Session::handshake_with(
        conn,
        &model,
        validator,
        minter,
        std::rc::Rc::new(std::cell::Cell::new(false)),
    )
    .unwrap();
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    // First presentation applies and focuses the window.
    client_write(
        &mut client,
        &Message::Command {
            id: 1,
            kind: CommandKind::ActivateWindow {
                window: id,
                token: ActivationToken::new(raw.clone()),
            },
        },
    );
    let handled = session.handle_next(&mut model).unwrap();
    assert_eq!(
        handled,
        Handled::CommandResult {
            id: 1,
            applied: true
        }
    );
    let Message::CommandResult { id: back, status } = client_read(&mut client) else {
        panic!("expected CommandResult for activation");
    };
    assert_eq!(back, 1);
    assert_eq!(status, CommandStatus::Applied);
    assert_eq!(model.focused(), Some(id));

    // Replay of the same token is denied; focus is untouched.
    client_write(
        &mut client,
        &Message::Command {
            id: 2,
            kind: CommandKind::ActivateWindow {
                window: id,
                token: ActivationToken::new(raw),
            },
        },
    );
    let handled = session.handle_next(&mut model).unwrap();
    assert_eq!(
        handled,
        Handled::CommandResult {
            id: 2,
            applied: false
        }
    );
    let Message::CommandResult { status, .. } = client_read(&mut client) else {
        panic!("expected CommandResult for replay");
    };
    assert!(matches!(status, CommandStatus::Denied { .. }));
    assert_eq!(model.focused(), Some(id));
}

#[test]
fn focus_workspace_and_toggle_overview() {
    let mut model = StateModel::new();
    let _ = model.insert("term", None, 1);
    let (conn, mut client) = pair();
    let mut session = handshake(conn, &mut client, &model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    for (id, kind, applied) in [
        (11u64, CommandKind::FocusWorkspace { workspace: 1 }, true),
        // Dynamic workspaces: unknown ids switch (registering if new).
        (12u64, CommandKind::FocusWorkspace { workspace: 999 }, true),
        (13u64, CommandKind::ToggleOverview, true),
        (
            14u64,
            CommandKind::FocusWorkspace {
                workspace: u64::MAX,
            },
            false,
        ),
    ] {
        client_write(&mut client, &Message::Command { id, kind });
        let handled = session.handle_next(&mut model).unwrap();
        assert_eq!(handled, Handled::CommandResult { id, applied });
        let Message::CommandResult { id: back, status } = client_read(&mut client) else {
            panic!("expected CommandResult");
        };
        assert_eq!(back, id);
        assert_eq!(status == CommandStatus::Applied, applied);
    }
    assert_eq!(model.active_workspace(), 999);
}

#[test]
fn focus_workspace_announces_active_and_resnapshots() {
    let mut model = StateModel::new();
    let _ = model.insert("term", None, 0);
    let (conn, mut client) = pair();
    let mut session = handshake(conn, &mut client, &model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    client_write(
        &mut client,
        &Message::Command {
            id: 1,
            kind: CommandKind::FocusWorkspace { workspace: 1 },
        },
    );
    assert_eq!(
        session.handle_next(&mut model).unwrap(),
        Handled::CommandResult {
            id: 1,
            applied: true
        }
    );
    assert!(matches!(
        client_read(&mut client),
        Message::CommandResult { .. }
    ));
    // The switch streams as a single active-exclusive delta op.
    let emitted = session.emit_deltas(&model).unwrap();
    assert!(matches!(emitted, Emitted::Changes { .. }));
    let Message::Changes { ops, .. } = client_read(&mut client) else {
        panic!("expected Changes after FocusWorkspace");
    };
    assert!(
        ops.iter().any(|op| matches!(
            op,
            StateOp::WorkspaceChanged(w) if w.id == 1 && w.active
        )),
        "unexpected ops: {ops:?}"
    );

    // A fresh handshake snapshots the moved active flag.
    let (conn2, mut client2) = pair();
    let _ = handshake(conn2, &mut client2, &model);
    assert!(matches!(client_read(&mut client2), Message::Hello { .. }));
    let Message::Snapshot { workspaces, .. } = client_read(&mut client2) else {
        panic!("expected Snapshot on re-handshake");
    };
    let (zero, one) = (
        workspaces.iter().find(|w| w.id == 0).unwrap(),
        workspaces.iter().find(|w| w.id == 1).unwrap(),
    );
    assert!(!zero.active);
    assert!(one.active);
}
