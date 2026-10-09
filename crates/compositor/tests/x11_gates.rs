//! Privilege-gate parity for X11 entries (xwayland-support gates task).
//!
//! Acceptance: unauthorized capture and layer-shell probes against a
//! legacy window fail exactly as for native windows. The gates
//! themselves are kind-agnostic by construction
//! ([`deny_all_tokens`](tuna_compositor::control::deny_all_tokens)
//! ignores both arguments; no screencopy or foreign-toplevel globals
//! exist), so these tests pin that property: the gate never branches
//! on window kind or identity source, `ActivateWindow` against an
//! X11-identity model entry is denied under the fail-closed default,
//! and no capture/toplevel-control global is advertised (so a future
//! protocol addition trips the test and forces a parity review).
//!
//! Conventions follow `control.rs` (socketpair session harness,
//! blocking client with timeouts, plain asserts) and `handshake.rs`
//! (socketpair protocol client, bounded roundtrip budget). All code
//! here is original.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use tuna_compositor::control::{deny_all_tokens, ControlConn, Handled, Session};
use tuna_compositor::state::StateModel;
use tuna_compositor::TestCompositor;
use tuna_shell_control::{
    decode_frame, encode_frame, ActivationToken, CommandKind, CommandStatus, Message,
    CURRENT_VERSION,
};
use wayland_client::{
    protocol::{wl_callback::WlCallback, wl_display::WlDisplay, wl_registry::WlRegistry},
    Connection, Dispatch, EventQueue, QueueHandle,
};

const TIMEOUT: Duration = Duration::from_secs(5);
const PUMP_ROUNDS: usize = 200;

/// Forged token: never issued by any store, so any acceptance would be
/// a gate bypass rather than a policy decision.
fn forged_token() -> ActivationToken {
    ActivationToken::new("forged".to_owned())
}

#[test]
fn deny_all_tokens_denies_regardless_of_window_kind() {
    // None: windows without any identity (either kind) are denied.
    assert!(!deny_all_tokens(&forged_token(), None));
    // Native-style app id.
    assert!(!deny_all_tokens(&forged_token(), Some("org.example.Term")));
    // X11-style WM_CLASS strings (class/instance spellings): the gate
    // must not branch on the identity source either.
    for app_id in ["XTerm", "xterm", "Firefox", "firefox", "Emacs", "GIMP"] {
        assert!(
            !deny_all_tokens(&forged_token(), Some(app_id)),
            "gate must deny X11-style identity {app_id:?} exactly as native"
        );
    }
}

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

/// Drive a full handshake; returns the session (fail-closed tokens).
fn handshake(conn: ControlConn, client: &mut UnixStream, model: &StateModel) -> Session<'static> {
    client_write(
        client,
        &Message::Hello {
            version: CURRENT_VERSION,
        },
    );
    Session::handshake(conn, model).unwrap()
}

#[test]
fn activate_x11_identity_window_denied_under_fail_closed() {
    // Model entry carrying an X11-style WM_CLASS identity (see
    // `map_x11_identity`): the unified entry must land in the same
    // fail-closed path as a native entry.
    let mut model = StateModel::new();
    let native = model.insert("term", Some("org.example.Term"), 1);
    let legacy = model.insert("xterm", Some("XTerm"), 1);
    let (conn, mut client) = pair();
    let mut session = handshake(conn, &mut client, &model);
    assert!(matches!(client_read(&mut client), Message::Hello { .. }));
    assert!(matches!(client_read(&mut client), Message::Snapshot { .. }));

    // Identical assertions for both entry kinds: forged token denied,
    // focus untouched.
    for (id, label) in [(native, "native"), (legacy, "X11")] {
        client_write(
            &mut client,
            &Message::Command {
                id,
                kind: CommandKind::ActivateWindow {
                    window: id,
                    token: forged_token(),
                },
            },
        );
        let handled = session.handle_next(&mut model).unwrap();
        assert_eq!(
            handled,
            Handled::CommandResult { id, applied: false },
            "{label} entry must be denied under the fail-closed default"
        );
        let Message::CommandResult { id: back, status } = client_read(&mut client) else {
            panic!("expected CommandResult for {label} activation");
        };
        assert_eq!(back, id);
        let CommandStatus::Denied { reason } = status else {
            panic!("fail-closed default must deny {label} entries, got {status:?}");
        };
        assert!(!reason.is_empty());
    }
    assert_eq!(model.focused(), None);
}

/// Raw protocol client that records every advertised global interface.
#[derive(Default)]
struct GlobalsClient {
    interfaces: Vec<String>,
    synced: bool,
}

impl Dispatch<WlRegistry, ()> for GlobalsClient {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global { interface, .. } = event {
            state.interfaces.push(interface);
        }
    }
}

impl Dispatch<WlCallback, ()> for GlobalsClient {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: <WlCallback as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            state.synced = true;
        }
    }
}

macro_rules! empty_dispatch {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for GlobalsClient {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

empty_dispatch!(WlDisplay);

/// Pump the server and drain the client queue until `done` or the round
/// budget runs out (handshake.rs shape).
fn roundtrip(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<GlobalsClient>,
    client: &mut GlobalsClient,
    done: impl Fn(&GlobalsClient) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            guard.read().unwrap();
        }
        queue.dispatch_pending(client).unwrap();
        if done(client) {
            return;
        }
    }
    panic!("roundtrip budget exhausted");
}

/// Interface substrings that would give a client capture or
/// toplevel-control reach. Substring matching keeps the pin
/// future-proof: any new capture/control global trips this test and
/// forces a parity review for both window kinds.
const FORBIDDEN_GLOBAL_SUBSTRINGS: &[&str] = &[
    "screencopy",
    "foreign_toplevel",
    "foreign-toplevel",
    "image_capture",
    "image_copy_capture",
];

#[test]
fn no_capture_or_toplevel_control_global_advertised() {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let mut comp = TestCompositor::new();
    comp.add_client(server_stream);

    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = GlobalsClient::default();
    conn.display().get_registry(&qh, ());

    // Wait for the known core globals: proof the dump arrived before
    // asserting on its contents.
    roundtrip(&mut comp, &mut queue, &mut client, |c| {
        ["wl_compositor", "xdg_wm_base", "zwlr_layer_shell_v1"]
            .iter()
            .all(|known| c.interfaces.iter().any(|seen| seen == known))
    });
    // A display sync flushes any stragglers so the enumeration is
    // complete, not a prefix.
    client.synced = false;
    conn.display().sync(&qh, ());
    roundtrip(&mut comp, &mut queue, &mut client, |c| c.synced);

    for interface in &client.interfaces {
        for forbidden in FORBIDDEN_GLOBAL_SUBSTRINGS {
            assert!(
                !interface.contains(forbidden),
                "capture/control global {interface:?} must not be advertised \
                 (probes must fail trivially for native and X11 windows alike)"
            );
        }
    }
}

#[test]
fn windows_never_appear_as_layer_surfaces_regardless_of_kind() {
    // Layer-shell probes against a legacy window must fail exactly as
    // for a native window: neither kind ever shows up in the
    // layer-surface inventory, so there is nothing kind-specific to
    // probe.
    let mut comp = TestCompositor::new();
    comp.state.set_output_size(1280, 800);
    let mut model = StateModel::new();
    let _native = model.insert("term", Some("org.example.Term"), 1);
    let _legacy = model.insert("xterm", Some("XTerm"), 1);
    assert!(comp.state.panel_surfaces().is_empty());
    assert!(comp.state.layer_surfaces().is_empty());
}
