//! Control-socket peer authentication (#30, #70).
//!
//! The control socket is privileged: a session can focus and close
//! windows, drive the overview, and engage the lock. These cases pin
//! who may hold one: only our user, only the supervised shell once one
//! runs, nobody between restarts, a bounded pending queue, and never a
//! shared directory.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use roost_compositor::control::{
    is_private_dir, our_uid, ControlHub, PeerCred, PeerGate, MAX_PENDING_PEERS,
};
use roost_compositor::state::{StateModel, TokenStore};
use roost_shell_control::{encode_frame, Message, ProtocolVersion};

const SEAT: &str = "seat0";

fn bind() -> (tempfile::TempDir, ControlHub, std::path::PathBuf) {
    let dir = private_tempdir();
    let path = dir.path().join("roost-test.control");
    let hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT).unwrap();
    (dir, hub, path)
}

fn hello(stream: &mut UnixStream) {
    use std::io::Write;
    let frame = encode_frame(&Message::Hello {
        version: ProtocolVersion::CURRENT,
    });
    stream.write_all(&frame).unwrap();
}

/// Drive enough rounds for accept, then handshake.
fn rounds(hub: &mut ControlHub, model: &mut StateModel, n: usize) {
    for _ in 0..n {
        hub.poll(model);
    }
}

/// True when the server closed our end without sending anything.
fn refused(stream: &mut UnixStream) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut buf = [0u8; 16];
    // EOF, or a reset when our unread Hello was discarded with the
    // socket; either way no byte of compositor state arrived.
    match stream.read(&mut buf) {
        Ok(0) => true,
        Err(e) => e.kind() == std::io::ErrorKind::ConnectionReset,
        Ok(_) => false,
    }
}

fn me() -> PeerCred {
    PeerCred {
        pid: std::process::id(),
        uid: our_uid(),
    }
}

#[test]
fn gate_decisions_fail_closed() {
    let uid = our_uid();
    let other_user = PeerCred {
        pid: 42,
        uid: uid.wrapping_add(1),
    };
    assert!(
        !PeerGate::SameUser.admits(Some(other_user), uid),
        "other uid"
    );
    assert!(!PeerGate::SameUser.admits(None, uid), "unknown creds");
    assert!(PeerGate::SameUser.admits(Some(me()), uid));
    assert!(PeerGate::Pid(me().pid).admits(Some(me()), uid));
    assert!(
        !PeerGate::Pid(me().pid + 1).admits(Some(me()), uid),
        "forged shell"
    );
    assert!(
        !PeerGate::Pid(42).admits(Some(other_user), uid),
        "pid match, wrong uid"
    );
    assert!(!PeerGate::Closed.admits(Some(me()), uid));
}

#[test]
fn unauthorized_same_user_client_is_refused_before_handshake() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    // The supervised shell is some other process.
    hub.set_peer_gate(PeerGate::Pid(std::process::id() + 1));
    let mut intruder = UnixStream::connect(&path).unwrap();
    hello(&mut intruder);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 0);
    assert!(hub.refused_count() >= 1);
    assert!(
        refused(&mut intruder),
        "intruder must see EOF, not a snapshot"
    );
}

#[test]
fn closed_gate_admits_nobody() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    hub.set_peer_gate(PeerGate::Closed);
    let mut client = UnixStream::connect(&path).unwrap();
    hello(&mut client);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 0);
    assert!(refused(&mut client));
}

#[test]
fn supervised_shell_pid_is_admitted_and_reconnects() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    hub.set_peer_gate(PeerGate::Pid(std::process::id()));
    for _ in 0..2 {
        let mut shell = UnixStream::connect(&path).unwrap();
        hello(&mut shell);
        rounds(&mut hub, &mut model, 3);
        assert_eq!(hub.session_count(), 1, "valid shell (re)connect admitted");
        drop(shell);
        rounds(&mut hub, &mut model, 3);
    }
}

#[test]
fn narrowing_the_gate_evicts_a_live_session() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    let mut client = UnixStream::connect(&path).unwrap();
    hello(&mut client);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);
    // The shell restarted under a new pid: the old peer loses access.
    hub.set_peer_gate(PeerGate::Pid(std::process::id() + 1));
    rounds(&mut hub, &mut model, 1);
    assert_eq!(hub.session_count(), 0);
}

#[test]
fn connection_flood_is_bounded_and_the_shell_still_gets_in() {
    let (_dir, mut hub, path) = bind();
    let mut model = StateModel::new();
    let silent: Vec<UnixStream> = (0..MAX_PENDING_PEERS * 4)
        .map(|_| UnixStream::connect(&path).unwrap())
        .collect();
    hub.poll(&mut model);
    assert!(hub.pending_count() <= MAX_PENDING_PEERS);
    assert!(hub.refused_count() >= (MAX_PENDING_PEERS * 3) as u64);
    // Silent peers age out after their one handshake attempt.
    rounds(&mut hub, &mut model, 3);
    drop(silent);
    let mut shell = UnixStream::connect(&path).unwrap();
    hello(&mut shell);
    rounds(&mut hub, &mut model, 3);
    assert_eq!(hub.session_count(), 1);
}

#[test]
fn bind_refuses_a_shared_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
    assert!(!is_private_dir(dir.path()));
    let Err(err) = ControlHub::bind(
        dir.path().join("roost-test.control"),
        Rc::new(TokenStore::new()),
        SEAT,
    ) else {
        panic!("bind in a shared dir must fail");
    };
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn socket_file_is_owner_only() {
    let (_dir, _hub, path) = bind();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// Temp dir with owner-only permissions: the control socket refuses to
/// bind anywhere less private (#30).
fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
