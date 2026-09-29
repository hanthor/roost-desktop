//! T3 live shell overview and activation wiring (hub end-to-end).
//!
//! Unlike `control.rs` (single `Session` over a socketpair), these tests
//! drive [`ControlHub`]: bind on a tempfile socket, map two real protocol
//! toplevels with distinct app_ids through [`WindowManager`], handshake a
//! raw control client over several hub rounds, and assert the live
//! snapshot plus token-gated activation. Conventions follow `windows.rs`
//! (socketpair protocol clients, bounded pump budgets, no sleeps) and
//! `control.rs` (blocking raw client with timeouts, plain asserts).
//! Panel surfaces, supervision, and overlay are other tasks' scope.
//! All code here is original.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use roost_compositor::control::ControlHub;
use roost_compositor::state::TokenStore;
use roost_compositor::windows::WindowManager;
use roost_compositor::{TestCompositor, SEAT_NAME};
use roost_shell_control::{
    decode_frame, encode_frame, ActivationToken, CommandKind, CommandStatus, Message,
    CURRENT_VERSION,
};
use wayland_client::{
    protocol::{
        wl_callback::WlCallback, wl_compositor::WlCompositor, wl_display::WlDisplay,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

const TIMEOUT: Duration = Duration::from_secs(5);
const PUMP_ROUNDS: usize = 200;
const HUB_ROUNDS: usize = 50;

/// Minimal protocol client: compositor + xdg shell only (no seat; these
/// tests map surfaces and read snapshots, never deliver input).
#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    xdg_base: Option<XdgWmBase>,
    surface: Option<WlSurface>,
    xdg_surface: Option<XdgSurface>,
    toplevel: Option<XdgToplevel>,
    synced: bool,
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some() && self.xdg_base.is_some()
    }
}

impl Dispatch<wayland_client::protocol::wl_registry::WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        event: <wayland_client::protocol::wl_registry::WlRegistry as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor =
                        Some(registry.bind::<WlCompositor, _, _>(name, version.min(6), qh, ()));
                }
                "xdg_wm_base" => {
                    state.xdg_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version.min(7), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for Client {
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

impl Dispatch<XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: <XdgWmBase as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure { serial } =
            event
        {
            xdg_surface.ack_configure(serial);
            if let Some(surface) = &state.surface {
                surface.commit();
            }
        }
    }
}

macro_rules! empty_dispatch {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for Client {
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
empty_dispatch!(WlCompositor);
empty_dispatch!(WlSurface);
empty_dispatch!(XdgToplevel);

/// Pump server and drain one client queue until `done` or budget out.
fn pump(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
    done: impl Fn(&Client) -> bool,
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
    panic!("pump budget exhausted");
}

fn connect(comp: &mut TestCompositor) -> (Connection, EventQueue<Client>, Client) {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server_stream);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    pump(comp, &mut queue, &mut client, |c| c.ready());
    (conn, queue, client)
}

/// Map one titled toplevel carrying an app id on an connected client.
fn map_toplevel(
    comp: &mut TestCompositor,
    conn: &Connection,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
    title: &str,
    app_id: &str,
) {
    let qh = queue.handle();
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg_surface = client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title(title.to_owned());
    toplevel.set_app_id(app_id.to_owned());
    surface.commit();
    client.surface = Some(surface);
    client.xdg_surface = Some(xdg_surface.clone());
    client.toplevel = Some(toplevel);
    client.synced = false;
    conn.display().sync(&qh, ());
    pump(comp, queue, client, |c| c.synced);
}

/// Blocking raw control client helpers (mirror `control.rs` conventions).
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

/// Read frames until the `CommandResult` for `id` arrives, skipping
/// interleaved `Changes` deltas the hub emits after model mutations and
/// `Overview` intents (UI state outside the ordered model stream).
fn read_result(client: &mut UnixStream, id: u64) -> CommandStatus {
    for _ in 0..HUB_ROUNDS {
        match client_read(client) {
            Message::CommandResult { id: back, status } if back == id => return status,
            Message::Changes { .. } | Message::Overview { .. } => continue,
            other => panic!("expected CommandResult for {id}, got {other:?}"),
        }
    }
    panic!("CommandResult for {id} never arrived");
}

/// Live hub over two mapped protocol windows, plus a handshook raw
/// control client holding the handshake snapshot.
struct Fixture {
    comp: TestCompositor,
    manager: WindowManager,
    hub: ControlHub,
    control: UnixStream,
    id_a: u64,
    id_b: u64,
    token_a: String,
    token_b: String,
    revision: u64,
    _socket_dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let mut comp = TestCompositor::new();
    // Manager first so seat capabilities exist before any client binds,
    // mirroring `Runtime::launch` (see `windows.rs`).
    let mut manager = comp.window_manager();

    let (conn_a, mut queue_a, mut client_a) = connect(&mut comp);
    map_toplevel(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        "alpha",
        "com.example.alpha",
    );
    let (conn_b, mut queue_b, mut client_b) = connect(&mut comp);
    map_toplevel(
        &mut comp,
        &conn_b,
        &mut queue_b,
        &mut client_b,
        "beta",
        "com.example.beta",
    );
    for _ in 0..PUMP_ROUNDS {
        comp.pump();
        if comp.state.toplevel_count() >= 2 {
            break;
        }
    }
    assert_eq!(comp.state.toplevel_count(), 2);
    manager.reconcile(&mut comp.state);
    assert_eq!(manager.model().windows().count(), 2);

    let id_a = manager
        .model()
        .windows()
        .find(|w| w.title == "alpha")
        .unwrap()
        .id;
    let id_b = manager
        .model()
        .windows()
        .find(|w| w.title == "beta")
        .unwrap()
        .id;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("control.sock");
    let mut hub = ControlHub::bind(path.clone(), Rc::new(TokenStore::new()), SEAT_NAME).unwrap();

    // Raw client speaks first: its Hello waits in the socket buffer while
    // the hub accepts (round 1) and handshakes (round 2).
    let mut control = UnixStream::connect(&path).unwrap();
    control
        .set_read_timeout(Some(TIMEOUT))
        .and(control.set_write_timeout(Some(TIMEOUT)))
        .unwrap();
    client_write(
        &mut control,
        &Message::Hello {
            version: CURRENT_VERSION,
        },
    );
    for _ in 0..HUB_ROUNDS {
        hub.poll(manager.model_mut());
        comp.pump();
        if hub.session_count() == 1 {
            break;
        }
    }
    assert_eq!(hub.session_count(), 1, "hub must handshake one session");

    let Message::Hello { version } = client_read(&mut control) else {
        panic!("expected Hello reply");
    };
    assert_eq!(version, CURRENT_VERSION);
    let Message::Snapshot {
        revision,
        windows,
        workspaces,
    } = client_read(&mut control)
    else {
        panic!("expected Snapshot after Hello");
    };
    assert_eq!(revision, manager.model().revision());
    assert_eq!(windows.len(), 2);
    let token_a = windows
        .iter()
        .find(|w| w.id == id_a)
        .unwrap()
        .activation_token
        .clone();
    let token_b = windows
        .iter()
        .find(|w| w.id == id_b)
        .unwrap()
        .activation_token
        .clone();
    assert!(!token_a.is_empty() && !token_b.is_empty());
    assert_ne!(token_a, token_b, "each window mints a distinct token");
    // Workspace truth travels with the snapshot.
    assert_eq!(workspaces.len(), 1);
    assert!(windows.iter().all(|w| w.workspace == 0));
    // Mapping focuses exactly one window.
    assert_eq!(
        windows.iter().filter(|w| w.focused).count(),
        1,
        "snapshot must mark exactly one focused window"
    );

    Fixture {
        comp,
        manager,
        hub,
        control,
        id_a,
        id_b,
        token_a,
        token_b,
        revision,
        _socket_dir: dir,
    }
}

#[test]
fn overview_snapshot_lists_live_windows_with_fresh_tokens_and_workspace_truth() {
    let f = fixture();
    assert_eq!(f.revision, f.manager.model().revision());
    let entry_a = f.manager.model().window(f.id_a).unwrap();
    let entry_b = f.manager.model().window(f.id_b).unwrap();
    assert_eq!(entry_a.app_id.as_deref(), Some("com.example.alpha"));
    assert_eq!(entry_b.app_id.as_deref(), Some("com.example.beta"));
    assert_eq!(f.manager.model().workspaces(), &[0]);
}

#[test]
fn activate_window_with_live_token_focuses_and_applies() {
    let mut f = fixture();
    // Present A's token for B first is covered elsewhere; here A for A.
    // B holds focus from mapping, so applying A's token must move focus.
    assert_ne!(f.manager.model().focused(), Some(f.id_a));
    client_write(
        &mut f.control,
        &Message::Command {
            id: 11,
            kind: CommandKind::ActivateWindow {
                window: f.id_a,
                token: ActivationToken::new(f.token_a.clone()),
            },
        },
    );
    let mut activated = Vec::new();
    for _ in 0..HUB_ROUNDS {
        activated.extend(f.hub.poll(f.manager.model_mut()).activated);
        f.comp.pump();
        if activated.contains(&f.id_a) {
            break;
        }
    }
    assert!(
        activated.contains(&f.id_a),
        "hub must report A's activation, got {activated:?}"
    );
    assert_eq!(read_result(&mut f.control, 11), CommandStatus::Applied);
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
}

#[test]
fn close_window_applies_and_reports_for_runtime() {
    let mut f = fixture();
    client_write(
        &mut f.control,
        &Message::Command {
            id: 21,
            kind: CommandKind::CloseWindow { window: f.id_a },
        },
    );
    let mut closed = Vec::new();
    for _ in 0..HUB_ROUNDS {
        closed.extend(f.hub.poll(f.manager.model_mut()).closed);
        f.comp.pump();
        if closed.contains(&f.id_a) {
            break;
        }
    }
    assert_eq!(closed, vec![f.id_a], "hub must report A's close");
    assert_eq!(read_result(&mut f.control, 21), CommandStatus::Applied);
}

#[test]
fn close_window_unknown_id_denies() {
    let mut f = fixture();
    client_write(
        &mut f.control,
        &Message::Command {
            id: 22,
            kind: CommandKind::CloseWindow { window: 999_999 },
        },
    );
    let mut closed = Vec::new();
    for _ in 0..HUB_ROUNDS {
        closed.extend(f.hub.poll(f.manager.model_mut()).closed);
        f.comp.pump();
    }
    assert!(closed.is_empty(), "unknown close must report nothing");
    let status = read_result(&mut f.control, 22);
    assert!(
        matches!(status, CommandStatus::Denied { .. }),
        "unknown close must deny, got {status:?}"
    );
}

#[test]
fn replayed_and_cross_window_tokens_are_denied() {
    let mut f = fixture();
    // Cross-window first: A's token for B denies on app mismatch without
    // consuming the token, so it stays valid for its own window after.
    client_write(
        &mut f.control,
        &Message::Command {
            id: 10,
            kind: CommandKind::ActivateWindow {
                window: f.id_b,
                token: ActivationToken::new(f.token_a.clone()),
            },
        },
    );
    let mut activated = Vec::new();
    for _ in 0..HUB_ROUNDS {
        activated.extend(f.hub.poll(f.manager.model_mut()).activated);
        f.comp.pump();
    }
    assert!(
        activated.is_empty(),
        "denied activation must report nothing, got {activated:?}"
    );
    let status = read_result(&mut f.control, 10);
    assert!(
        matches!(status, CommandStatus::Denied { .. }),
        "cross-window token must deny, got {status:?}"
    );
    let focused_before = f.manager.model().focused();

    // Now the same token works once for its own window ...
    client_write(
        &mut f.control,
        &Message::Command {
            id: 11,
            kind: CommandKind::ActivateWindow {
                window: f.id_a,
                token: ActivationToken::new(f.token_a.clone()),
            },
        },
    );
    let mut activated = Vec::new();
    for _ in 0..HUB_ROUNDS {
        activated.extend(f.hub.poll(f.manager.model_mut()).activated);
        f.comp.pump();
        if activated.contains(&f.id_a) {
            break;
        }
    }
    assert_eq!(read_result(&mut f.control, 11), CommandStatus::Applied);

    // ... and a replay of the consumed token denies.
    client_write(
        &mut f.control,
        &Message::Command {
            id: 12,
            kind: CommandKind::ActivateWindow {
                window: f.id_a,
                token: ActivationToken::new(f.token_a.clone()),
            },
        },
    );
    for _ in 0..HUB_ROUNDS {
        f.hub.poll(f.manager.model_mut());
        f.comp.pump();
    }
    let status = read_result(&mut f.control, 12);
    assert!(
        matches!(status, CommandStatus::Denied { .. }),
        "replayed token must deny, got {status:?}"
    );
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    assert_ne!(
        focused_before,
        Some(f.id_a),
        "cross attempt must not move focus"
    );

    // B's own token stayed valid through all the A traffic: presenting it
    // moves focus back to B.
    client_write(
        &mut f.control,
        &Message::Command {
            id: 13,
            kind: CommandKind::ActivateWindow {
                window: f.id_b,
                token: ActivationToken::new(f.token_b.clone()),
            },
        },
    );
    let mut activated = Vec::new();
    for _ in 0..HUB_ROUNDS {
        activated.extend(f.hub.poll(f.manager.model_mut()).activated);
        f.comp.pump();
        if activated.contains(&f.id_b) {
            break;
        }
    }
    assert_eq!(read_result(&mut f.control, 13), CommandStatus::Applied);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
}
