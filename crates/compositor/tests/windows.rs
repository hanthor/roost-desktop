//! T2 window mapping, input routing, and focus-policy tests.
//!
//! Headless end-to-end coverage for `roost_compositor::windows` with no
//! backend: real protocol clients over a socketpair map xdg_toplevels,
//! then the test drives [`WindowManager`] directly (reconcile, focus,
//! pointer/keyboard delivery, move/resize/workspace) and observes the
//! results model-side and through client listeners.
//!
//! Conventions follow `handshake.rs`: a single-threaded pump loop with a
//! bounded round budget, plain asserts, no sleeps. Every client read is
//! preceded by a server message that guarantees it (configure, sync
//! reply, key/pointer event), so no read can block. All code here is
//! original.

use std::os::unix::net::UnixStream;

use roost_compositor::layer::OVERVIEW_NAMESPACE;
use roost_compositor::windows::{
    ManagerInput, SessionMode, TileSide, WindowLayout, WindowManager, ALT_LEFT_KEYCODE,
    ARROW_DOWN_KEYCODE, ARROW_LEFT_KEYCODE, ARROW_RIGHT_KEYCODE, ARROW_UP_KEYCODE,
    CTRL_LEFT_KEYCODE, ESCAPE_KEYCODE, F4_KEYCODE, GRAVE_KEYCODE, PAGE_DOWN_KEYCODE,
    PAGE_UP_KEYCODE, R_KEYCODE, SHIFT_LEFT_KEYCODE, SUPER_LEFT_KEYCODE, TAB_KEYCODE, T_KEYCODE,
};
use roost_compositor::TestCompositor;
use roost_shell_control::SwitcherAction;
use smithay::desktop::WindowSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{IsAlive, Logical, Point};
use smithay::wayland::seat::WaylandFocus;
use wayland_client::{
    protocol::{
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_display::WlDisplay,
        wl_keyboard::{Event as KeyEvent, KeyState as ClientKeyState, WlKeyboard},
        wl_pointer::{ButtonState as ClientButtonState, Event as PointerEvent, WlPointer},
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{
        Anchor, Event as LayerSurfaceEvent, KeyboardInteractivity, ZwlrLayerSurfaceV1,
    },
};

const PUMP_ROUNDS: usize = 200;
/// Button code for BTN_LEFT, sent in the pointer-button test.
const BTN_LEFT: u32 = 0x110;

/// One protocol client: bound globals plus the mapped surface and every
/// observation the tests assert on.
#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    xdg_base: Option<XdgWmBase>,
    surface: Option<WlSurface>,
    xdg_surface: Option<XdgSurface>,
    toplevel: Option<XdgToplevel>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    layer_shell: Option<ZwlrLayerShellV1>,
    overview_layer: Option<ZwlrLayerSurfaceV1>,
    synced: bool,
    configured: bool,
    /// (keycode, pressed) in arrival order.
    keys: Vec<(u32, bool)>,
    pointer_enters: u32,
    pointer_motions: u32,
    /// (button, pressed) in arrival order.
    pointer_buttons: Vec<(u32, bool)>,
    /// Every advertised toplevel size, in arrival order.
    configure_sizes: Vec<(i32, i32)>,
    /// Every advertised toplevel state array (raw wire bytes), in
    /// arrival order.
    configure_states: Vec<Vec<u8>>,
    /// Whether the server asked this client to close.
    close_requested: bool,
}

/// Whether a raw xdg-toplevel state array carries `want` (native-endian
/// uints on the wire: 1=maximized, 2=fullscreen, 4=activated per the
/// xdg-shell protocol enum).
fn has_state(states: &[u8], want: u32) -> bool {
    let (chunks, _) = states.as_chunks::<4>();
    chunks
        .iter()
        .any(|chunk| u32::from_ne_bytes(*chunk) == want)
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some()
            && self.seat.is_some()
            && self.xdg_base.is_some()
            && self.keyboard.is_some()
            && self.pointer.is_some()
    }
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
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
                "wl_seat" => {
                    // The test seat always offers keyboard and pointer, so
                    // both are acquired up front rather than on capabilities.
                    let seat = registry.bind::<WlSeat, _, _>(name, version, qh, ());
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                    state.pointer = Some(seat.get_pointer(qh, ()));
                    state.seat = Some(seat);
                }
                "xdg_wm_base" => {
                    state.xdg_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version.min(7), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell =
                        Some(registry.bind::<ZwlrLayerShellV1, _, _>(name, version.min(5), qh, ()));
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
            state.configured = true;
            if let Some(surface) = &state.surface {
                surface.commit();
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: <XdgToplevel as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match &event {
            wayland_protocols::xdg::shell::client::xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                state.configure_sizes.push((*width, *height));
                state.configure_states.push(states.clone());
            }
            wayland_protocols::xdg::shell::client::xdg_toplevel::Event::Close => {
                state.close_requested = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let KeyEvent::Key {
            key,
            state: key_state,
            ..
        } = event
        {
            state.keys.push((
                key,
                matches!(key_state, WEnum::Value(ClientKeyState::Pressed)),
            ));
        }
    }
}

impl Dispatch<WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: <WlPointer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            PointerEvent::Enter { .. } => state.pointer_enters += 1,
            PointerEvent::Motion { .. } => state.pointer_motions += 1,
            PointerEvent::Button {
                button,
                state: button_state,
                ..
            } => state.pointer_buttons.push((
                button,
                matches!(button_state, WEnum::Value(ClientButtonState::Pressed)),
            )),
            _ => {}
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
empty_dispatch!(WlSeat);
empty_dispatch!(ZwlrLayerShellV1);

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        _: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let LayerSurfaceEvent::Configure { serial, .. } = event {
            layer.ack_configure(serial);
        }
    }
}

/// Pump the server and drain one client queue until `done` or the round
/// budget runs out. Single-threaded by design: neither side blocks, and
/// every call site guarantees a server reply first.
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

/// Connect one client and bind globals (compositor, seat with keyboard
/// and pointer, xdg shell).
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

/// Map one titled toplevel on an already-connected client.
fn map_toplevel(
    comp: &mut TestCompositor,
    conn: &Connection,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
    title: &str,
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
    surface.commit();
    client.surface = Some(surface);
    client.xdg_surface = Some(xdg_surface.clone());
    client.toplevel = Some(toplevel);
    client.configured = false;
    client.synced = false;
    conn.display().sync(&qh, ());
    // Only the sync barrier is awaited here: the initial configure is not
    // sent by the server until `WindowManager::reconcile` maps the
    // surface (which calls `send_configure`), so waiting for it now
    // would deadlock. The configure round-trip drains in `sync_client`
    // after reconcile.
    pump(comp, queue, client, |c| c.synced);
}

/// Drain one client's pending server messages behind a sync barrier.
fn sync_client(
    comp: &mut TestCompositor,
    conn: &Connection,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
) {
    let qh = queue.handle();
    client.synced = false;
    conn.display().sync(&qh, ());
    pump(comp, queue, client, |c| c.synced);
}

/// Two mapped windows ("alpha" from client A, "beta" from client B) with
/// a reconciled manager. Geometry: alpha at (0,0), beta cascaded to
/// (50,50), both 800x600.
struct Fixture {
    comp: TestCompositor,
    manager: WindowManager,
    conn_a: Connection,
    queue_a: EventQueue<Client>,
    client_a: Client,
    conn_b: Connection,
    queue_b: EventQueue<Client>,
    client_b: Client,
    id_a: u64,
    id_b: u64,
}

fn two_windows() -> Fixture {
    let mut comp = TestCompositor::new();
    // The manager first: its seat capabilities must exist before the
    // clients' `get_keyboard`/`get_pointer` requests arrive, or smithay
    // leaves those resources permanently inactive. This mirrors
    // `Runtime::launch`, which builds the manager before accepting
    // clients.
    let mut manager = comp.window_manager();
    let (conn_a, mut queue_a, mut client_a) = connect(&mut comp);
    map_toplevel(&mut comp, &conn_a, &mut queue_a, &mut client_a, "alpha");
    let (conn_b, mut queue_b, mut client_b) = connect(&mut comp);
    map_toplevel(&mut comp, &conn_b, &mut queue_b, &mut client_b, "beta");
    manager.reconcile(&mut comp.state);
    sync_client(&mut comp, &conn_a, &mut queue_a, &mut client_a);
    sync_client(&mut comp, &conn_b, &mut queue_b, &mut client_b);
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
    Fixture {
        comp,
        manager,
        conn_a,
        queue_a,
        client_a,
        conn_b,
        queue_b,
        client_b,
        id_a,
        id_b,
    }
}

/// A point inside alpha (0,0-800,600) but outside beta (50,50-850,650).
fn alpha_only() -> Point<f64, Logical> {
    (10.0, 10.0).into()
}

/// A point inside beta but outside alpha (x >= 800).
fn beta_only() -> Point<f64, Logical> {
    (810.0, 100.0).into()
}

#[test]
fn two_toplevels_map_with_cascaded_geometry() {
    let f = two_windows();
    assert_eq!(f.manager.model().windows().count(), 2);

    let geo_a = f.manager.geometry(f.id_a).unwrap();
    let geo_b = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((geo_a.size.w, geo_a.size.h), (800, 600));
    assert_eq!((geo_b.size.w, geo_b.size.h), (800, 600));
    let mut locs = vec![(geo_a.loc.x, geo_a.loc.y), (geo_b.loc.x, geo_b.loc.y)];
    locs.sort();
    assert_eq!(locs, vec![(0, 0), (50, 50)]);

    // Mapping focuses: exactly one window holds keyboard focus.
    let focused = f.manager.model().focused().unwrap();
    assert!(focused == f.id_a || focused == f.id_b);
    assert_eq!(f.manager.visible_windows().len(), 2);
}

#[test]
fn focus_follows_clicks_not_motion() {
    // GNOME's default click-to-focus: hovering never focuses or raises.
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    f.manager
        .pointer_motion(&mut f.comp.state, beta_only(), 1000);
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 1001);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 1002);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    f.manager
        .pointer_motion(&mut f.comp.state, alpha_only(), 1003);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
}

#[test]
fn focus_api_targets_rejects_and_clears() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    // Unknown ids are rejected and leave focus untouched.
    assert!(!f.manager.focus(&mut f.comp.state, Some(999)));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    assert!(f.manager.focus(&mut f.comp.state, None));
    assert_eq!(f.manager.model().focused(), None);
}

#[test]
fn focused_client_receives_keyboard_input() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_a.keys.clear();
    f.client_b.keys.clear();

    // Smithay takes XKB codespace and sends `raw - 8` on the wire,
    // so the client observes the evdev keycode we fed in.
    const KEY_IN: u32 = 30;
    const KEY_WIRE: u32 = KEY_IN;
    assert!(f
        .manager
        .keyboard_key(&mut f.comp.state, KEY_IN, true, 2000));
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        !c.keys.is_empty()
    });
    assert!(f
        .manager
        .keyboard_key(&mut f.comp.state, KEY_IN, false, 2001));
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.keys.len() >= 2
    });

    assert_eq!(f.client_b.keys, vec![(KEY_WIRE, true), (KEY_WIRE, false)]);
    assert!(
        f.client_a.keys.is_empty(),
        "unfocused client must receive no keys: {:?}",
        f.client_a.keys
    );
}

#[test]
fn pointer_client_receives_motion_and_button() {
    let mut f = two_windows();
    f.manager
        .pointer_motion(&mut f.comp.state, beta_only(), 3000);
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.pointer_enters > 0
    });
    assert!(f.client_b.pointer_enters > 0);
    // A fresh enter carries no trailing motion event; a second motion on
    // the same target produces one.
    f.manager
        .pointer_motion(&mut f.comp.state, (811.0, 101.0).into(), 3001);
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.pointer_motions > 0
    });
    assert!(f.client_b.pointer_motions > 0);

    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 3002);
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        !c.pointer_buttons.is_empty()
    });
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 3003);
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.pointer_buttons.len() >= 2
    });
    assert_eq!(
        f.client_b.pointer_buttons,
        vec![(BTN_LEFT, true), (BTN_LEFT, false)]
    );
    // Click-to-focus: the press lands on beta under the cursor.
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    // Motion over beta never reaches alpha's client.
    assert_eq!(f.client_a.pointer_motions, 0);
    assert!(f.client_a.pointer_buttons.is_empty());
}

#[test]
fn move_and_resize_update_geometry_and_advertise_size() {
    let mut f = two_windows();
    assert!(f.manager.move_window(f.id_b, 50, 60));
    let moved = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((moved.loc.x, moved.loc.y), (100, 110));
    assert!(!f.manager.move_window(999, 1, 1));

    assert!(f.manager.resize_window(f.id_b, 400, 300));
    let resized = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((resized.size.w, resized.size.h), (400, 300));
    assert!(!f.manager.resize_window(f.id_b, 0, 100));
    assert!(!f.manager.resize_window(999, 400, 300));

    // The resize is advertised: the client observes the new size.
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.configure_sizes.last() == Some(&(400, 300))
    });
}

#[test]
fn move_to_workspace_updates_model() {
    let mut f = two_windows();
    assert!(f.manager.move_to_workspace(&mut f.comp.state, f.id_a, 3));
    assert_eq!(f.manager.model().window(f.id_a).unwrap().workspace, 3);
    // Untouched windows keep their workspace.
    assert_eq!(f.manager.model().window(f.id_b).unwrap().workspace, 0);
    assert!(!f.manager.move_to_workspace(&mut f.comp.state, 999, 3));
}

#[test]
fn window_menu_actions_follow_gnome() {
    use roost_shell_control::WindowAction;
    let mut f = two_windows();
    // Always on Visible Workspace: shown on the next workspace too.
    assert!(f
        .manager
        .window_action(&mut f.comp.state, f.id_a, WindowAction::ToggleSticky));
    assert!(f.manager.is_sticky(f.id_a));
    assert!(f.manager.switch_relative(&mut f.comp.state, 1));
    assert_eq!(
        f.manager.visible_windows().len(),
        1,
        "only the sticky window shows"
    );
    assert!(f.manager.switch_relative(&mut f.comp.state, -1));
    // Hide takes it off the screen; activating it brings it back.
    assert!(f
        .manager
        .window_action(&mut f.comp.state, f.id_b, WindowAction::Minimize));
    assert!(f.manager.is_minimized(f.id_b));
    assert_eq!(f.manager.visible_windows().len(), 1);
    assert_ne!(f.manager.model().focused(), Some(f.id_b));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    assert!(!f.manager.is_minimized(f.id_b));
    // Move to Workspace: right, then left; none to the left of the first.
    assert!(f.manager.window_action(
        &mut f.comp.state,
        f.id_b,
        WindowAction::MoveToWorkspaceRight
    ));
    assert_eq!(f.manager.model().window(f.id_b).unwrap().workspace, 1);
    assert!(f.manager.workspace_left_of(f.id_b));
    assert!(f
        .manager
        .window_action(&mut f.comp.state, f.id_b, WindowAction::MoveToWorkspaceLeft));
    assert_eq!(f.manager.model().window(f.id_b).unwrap().workspace, 0);
    assert!(!f.manager.workspace_left_of(f.id_b));
    assert!(!f
        .manager
        .window_action(&mut f.comp.state, f.id_b, WindowAction::MoveToWorkspaceLeft));
    // Always on Top stays above a newly focused window.
    assert!(f
        .manager
        .window_action(&mut f.comp.state, f.id_b, WindowAction::ToggleAbove));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    let top = f.manager.visible_windows().last().map(|(w, _)| w.clone());
    let b_surface = f.manager.surface_of(f.id_b);
    assert_eq!(
        top.and_then(|w| w.wl_surface().map(|s| s.into_owned())),
        b_surface
    );
}

#[test]
fn switch_relative_creates_next_and_clamps_at_first() {
    let mut f = two_windows();
    // +1 past the only workspace creates id 1; it is empty, so focus
    // clears while both windows stay mapped-but-hidden on 0.
    assert!(f.manager.switch_relative(&mut f.comp.state, 1));
    assert_eq!(f.manager.model().active_workspace(), 1);
    assert_eq!(f.manager.model().focused(), None);
    assert_eq!(f.manager.model().workspaces(), &[0, 1]);
    assert!(f.manager.visible_windows().is_empty());
    // -1 returns to 0 and refocuses its topmost window.
    assert!(f.manager.switch_relative(&mut f.comp.state, -1));
    assert_eq!(f.manager.model().active_workspace(), 0);
    assert!(f.manager.model().focused().is_some());
    assert_eq!(f.manager.visible_windows().len(), 2);
    // -1 at the first workspace is a no-op.
    assert!(!f.manager.switch_relative(&mut f.comp.state, -1));
    assert_eq!(f.manager.model().active_workspace(), 0);
    // Unknown direct ids are rejected: dynamic creation arrives via the
    // hub's FocusWorkspace command, not the manager API.
    assert!(!f.manager.switch_workspace(&mut f.comp.state, 77));
}

#[test]
fn move_focused_relative_moves_window_and_follows() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    assert!(f.manager.move_focused_relative(&mut f.comp.state, 1));
    assert_eq!(f.manager.model().window(f.id_b).unwrap().workspace, 1);
    assert_eq!(f.manager.model().active_workspace(), 1);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    // Only beta renders now; alpha stays mapped-but-hidden on 0.
    let visible = f.manager.visible_windows();
    assert_eq!(visible.len(), 1);
    // Hidden windows are not hit-testable: motion over alpha's old spot
    // (outside beta's geometry) leaves focus on beta.
    f.manager
        .pointer_motion(&mut f.comp.state, alpha_only(), 4000);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    // No focused window: the move is rejected.
    assert!(f.manager.focus(&mut f.comp.state, None));
    assert!(!f.manager.move_focused_relative(&mut f.comp.state, 1));
}

fn press(manager: &mut WindowManager, comp: &mut TestCompositor, keycode: u32) {
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode,
            pressed: true,
            time: 5000,
        },
    );
}

fn release(manager: &mut WindowManager, comp: &mut TestCompositor, keycode: u32) {
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode,
            pressed: false,
            time: 5001,
        },
    );
}

/// Evdev `a`: a bare key proving delivery by who records it.
const KEY_A: u32 = 30;

/// One pump round across three clients (single `pump` calls run one
/// round each, so three sequential calls drain one shared tick).
#[allow(clippy::too_many_arguments)]
fn drain3(
    comp: &mut TestCompositor,
    queue_a: &mut EventQueue<Client>,
    client_a: &mut Client,
    queue_b: &mut EventQueue<Client>,
    client_b: &mut Client,
    queue_c: &mut EventQueue<Client>,
    client_c: &mut Client,
) {
    pump(comp, queue_a, client_a, |_| true);
    pump(comp, queue_b, client_b, |_| true);
    pump(comp, queue_c, client_c, |_| true);
}

#[test]
fn super_page_keys_switch_and_shift_moves_focused() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();

    // Super+PageDown switches to a fresh workspace 1.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, PAGE_DOWN_KEYCODE);
    release(&mut f.manager, &mut f.comp, PAGE_DOWN_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(f.manager.model().active_workspace(), 1);
    // The switch press is consumed: the client never observes PageDown
    // (wire carries the evdev code). Only the Super press arrives: the
    // releases land after the switch strands focus on the empty
    // workspace, so waiting for a release would stall the pump.
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        !c.keys.is_empty()
    });
    assert!(
        !f.client_b
            .keys
            .iter()
            .any(|(key, _)| *key == PAGE_DOWN_KEYCODE),
        "PageDown must be consumed, got: {:?}",
        f.client_b.keys
    );

    // Super+Shift+PageUp moves beta back to 0 and follows it.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, PAGE_UP_KEYCODE);
    release(&mut f.manager, &mut f.comp, PAGE_UP_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(f.manager.model().window(f.id_b).unwrap().workspace, 0);
    assert_eq!(f.manager.model().active_workspace(), 0);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
}

#[test]
fn super_tab_switches_applications_like_alt_tab() {
    // GNOME binds switch-applications to Super+Tab as well as Alt+Tab;
    // Super's release commits, and no overview toggles.
    let mut f = two_windows();
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    release(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Step { forward: true }]
    );
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Commit]
    );
}

#[test]
fn rebound_switcher_keys_replace_the_builtin_chords() {
    // GNOME's switch-applications rebound to Ctrl+J: the chord steps,
    // Shift steps back, Control's release commits, and Alt+Tab is
    // ordinary input again. Super+Above_Tab stays a group step.
    use roost_shell_control::{SwitcherKey, SwitcherKeyKind, KEYSYM_ABOVE_TAB, MOD_CTRL, MOD_LOGO};
    const J_KEYCODE: u32 = 36;
    let mut f = two_windows();
    f.manager.set_switcher_keys(vec![
        SwitcherKey {
            keysym: 0x6a,
            mods: MOD_CTRL,
            kind: SwitcherKeyKind::Applications,
        },
        SwitcherKey {
            keysym: KEYSYM_ABOVE_TAB,
            mods: MOD_LOGO,
            kind: SwitcherKeyKind::Group,
        },
    ]);
    press(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    release(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    release(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    assert!(f.manager.take_switcher_queue().is_empty());

    press(&mut f.manager, &mut f.comp, CTRL_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, J_KEYCODE);
    release(&mut f.manager, &mut f.comp, J_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, J_KEYCODE);
    release(&mut f.manager, &mut f.comp, J_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![
            SwitcherAction::Step { forward: true },
            SwitcherAction::Step { forward: false }
        ]
    );
    release(&mut f.manager, &mut f.comp, CTRL_LEFT_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Commit]
    );

    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, GRAVE_KEYCODE);
    release(&mut f.manager, &mut f.comp, GRAVE_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![
            SwitcherAction::StepWindow { forward: true },
            SwitcherAction::Commit
        ]
    );
}

#[test]
fn alt_tab_steps_commits_and_cancels_without_leaking_keys() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();

    // Tab without Alt is ordinary input: forwarded, nothing queued.
    press(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    assert!(f.manager.take_switcher_queue().is_empty());
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        !c.keys.is_empty()
    });
    assert_eq!(f.client_b.keys.len(), 1);
    release(&mut f.manager, &mut f.comp, TAB_KEYCODE);

    // Alt+Tab queues a forward step and consumes the Tab press.
    // Drain the in-flight plain-Tab release first so the consumed-key
    // assertion below only sees chord traffic.
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();
    press(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Step { forward: true }]
    );
    // Shift+Tab steps back.
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    release(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Step { forward: false }]
    );
    // No Tab event reached the client; the Alt press did.
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert!(
        !f.client_b.keys.iter().any(|(key, _)| *key == TAB_KEYCODE),
        "Tab must be consumed, got: {:?}",
        f.client_b.keys
    );

    // Alt release commits the open session but still forwards, so app
    // modifiers never stick.
    let before = f.client_b.keys.len();
    release(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Commit]
    );
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert_eq!(f.client_b.keys.len(), before + 1);

    // Escape with no open session is ordinary input: forwarded, no
    // Cancel queued.
    press(&mut f.manager, &mut f.comp, ESCAPE_KEYCODE);
    assert!(f.manager.take_switcher_queue().is_empty());
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert_eq!(f.client_b.keys.len(), before + 2);
    release(&mut f.manager, &mut f.comp, ESCAPE_KEYCODE);

    // Escape with an open session cancels and is consumed.
    press(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Step { forward: true }]
    );
    // Drain first: the Alt press above is still in flight, and only a
    // synced count proves the cancel press adds nothing.
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    let before_cancel = f.client_b.keys.len();
    press(&mut f.manager, &mut f.comp, ESCAPE_KEYCODE);
    assert_eq!(
        f.manager.take_switcher_queue(),
        vec![SwitcherAction::Cancel]
    );
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert_eq!(f.client_b.keys.len(), before_cancel);
    release(&mut f.manager, &mut f.comp, ESCAPE_KEYCODE);
    release(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    // Alt release after a cancel commits nothing.
    assert!(f.manager.take_switcher_queue().is_empty());
}

/// Two windows on a 1280x800 output for layout tests.
fn layout_windows() -> Fixture {
    let mut f = two_windows();
    f.comp.state.set_output_size(1280, 800);
    f
}

#[test]
fn maximize_fills_work_area_and_restores() {
    let mut f = layout_windows();
    let before = f.manager.geometry(f.id_a).unwrap();
    assert!(f.manager.set_maximized(&mut f.comp.state, f.id_a, true));
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Maximized)
    );
    let maxed = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((maxed.loc.x, maxed.loc.y), (0, 32));
    assert_eq!((maxed.size.w, maxed.size.h), (1280, 768));
    // The client is told the work-area size with the Maximized state.
    pump(&mut f.comp, &mut f.queue_a, &mut f.client_a, |c| {
        c.configure_sizes.last() == Some(&(1280, 768))
    });
    assert!(
        f.client_a
            .configure_states
            .last()
            .is_some_and(|states| has_state(states, 1)),
        "expected Maximized advertised, got: {:?}",
        f.client_a.configure_states.last()
    );
    // Restore returns the stashed floating geometry.
    assert!(f.manager.set_maximized(&mut f.comp.state, f.id_a, false));
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Floating)
    );
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), before);
    assert!(!f.manager.set_maximized(&mut f.comp.state, 999, true));
}

#[test]
fn tile_halves_toggle_and_fullscreen_cover() {
    let mut f = layout_windows();
    assert!(f
        .manager
        .set_tiled(&mut f.comp.state, f.id_a, TileSide::Left));
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Tiled(TileSide::Left))
    );
    let left = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((left.loc.x, left.loc.y), (0, 32));
    assert_eq!((left.size.w, left.size.h), (640, 768));
    assert!(f
        .manager
        .set_tiled(&mut f.comp.state, f.id_a, TileSide::Right));
    let right = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((right.loc.x, right.loc.y), (640, 32));
    assert_eq!((right.size.w, right.size.h), (640, 768));
    // Moving between managed layouts keeps the original restore.
    assert!(f.manager.restore_window(f.id_a));
    assert_eq!(f.manager.geometry(f.id_a).unwrap().loc.x, 0);

    assert!(f.manager.set_fullscreen(&mut f.comp.state, f.id_b, true));
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Fullscreen)
    );
    let full = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((full.loc.x, full.loc.y), (0, 0));
    assert_eq!((full.size.w, full.size.h), (1280, 800));
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.configure_states
            .last()
            .is_some_and(|states| has_state(states, 2))
    });
    assert!(f.manager.set_fullscreen(&mut f.comp.state, f.id_b, false));
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
}

#[test]
fn manual_move_from_managed_layout_restores_first() {
    let mut f = layout_windows();
    assert!(f
        .manager
        .set_tiled(&mut f.comp.state, f.id_a, TileSide::Left));
    // Drag-off shape: back to the stashed floating geometry plus delta.
    assert!(f.manager.move_window(f.id_a, 10, 20));
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Floating)
    );
    let moved = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((moved.loc.x, moved.loc.y), (10, 20));
    assert_eq!((moved.size.w, moved.size.h), (800, 600));
}

#[test]
fn client_maximize_request_applies_on_reconcile() {
    let mut f = layout_windows();
    f.client_b.toplevel.as_ref().unwrap().set_maximized();
    f.queue_b.flush().unwrap();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Maximized)
    );
    assert_eq!(f.manager.geometry(f.id_b).unwrap().size.h, 768);
    // And back off again through the protocol.
    f.client_b.toplevel.as_ref().unwrap().unset_maximized();
    f.queue_b.flush().unwrap();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
}

#[test]
fn transient_dialog_centers_above_parent() {
    let mut f = layout_windows();
    // Beta sits cascaded at (50,50); the dialog centers on it, which a
    // plain cascade to (64,64) would never produce. The dialog shares
    // beta's connection: Wayland object ids are connection-scoped, so
    // a cross-client parent is a protocol error, not a placement.
    let qh = f.queue_b.handle();
    let surface = f
        .client_b
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let dialog_xdg = f
        .client_b
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let dialog_top = dialog_xdg.get_toplevel(&qh, ());
    dialog_top.set_title("dialog".to_owned());
    dialog_top.set_parent(f.client_b.toplevel.as_ref());
    surface.commit();
    f.conn_b.display().sync(&qh, ());
    // The sync barrier is enough: the server processed the dialog
    // commit (and its parent) before answering it.
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| c.synced);
    f.manager.reconcile(&mut f.comp.state);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    let _keep_alive = (surface, dialog_xdg, dialog_top);

    let id_c = f
        .manager
        .model()
        .windows()
        .find(|w| w.title == "dialog")
        .unwrap()
        .id;
    // Dialog takes focus and stacks directly above its parent.
    assert_eq!(f.manager.model().focused(), Some(id_c));
    // Directly above beta: last two in stacking order are beta, dialog.
    let visible = f.manager.visible_windows();
    assert_eq!(visible.len(), 3);
    let geo = f.manager.geometry(id_c).unwrap();
    assert_eq!((geo.loc.x, geo.loc.y), (50, 50));
}

#[test]
fn super_arrows_drive_layouts_and_consume() {
    let mut f = layout_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();

    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_UP_KEYCODE);
    release(&mut f.manager, &mut f.comp, ARROW_UP_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Maximized)
    );
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Tiled(TileSide::Left))
    );
    // Repeat toggles back to floating.
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
    press(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    release(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Tiled(TileSide::Right))
    );
    press(&mut f.manager, &mut f.comp, ARROW_DOWN_KEYCODE);
    release(&mut f.manager, &mut f.comp, ARROW_DOWN_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    // No arrow press reached the client; releases still did (modifiers
    // must not stick), so only releases are observed.
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert!(
        !f.client_b.keys.iter().any(|(key, pressed)| [
            ARROW_UP_KEYCODE,
            ARROW_DOWN_KEYCODE,
            ARROW_LEFT_KEYCODE,
            ARROW_RIGHT_KEYCODE
        ]
        .contains(key)
            && *pressed),
        "arrow presses must be consumed, got: {:?}",
        f.client_b.keys
    );
}

#[test]
fn alt_f4_politely_closes_focused_window() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();

    // Alt+F4 asks beta to close and consumes the press.
    press(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, F4_KEYCODE);
    release(&mut f.manager, &mut f.comp, F4_KEYCODE);
    release(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    assert!(f.manager.take_switcher_queue().is_empty());
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.close_requested
    });
    assert!(f.client_b.close_requested);
    assert!(
        !f.client_b.keys.iter().any(|(key, _)| *key == F4_KEYCODE),
        "F4 press must be consumed, got: {:?}",
        f.client_b.keys
    );

    // The client honors the request by going away; reconcile drops it
    // and focus falls back to alpha.
    drop((f.conn_b, f.queue_b, f.client_b));
    f.comp.pump();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.model().window(f.id_b).is_none());
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    assert!(!f.manager.close_window(999));
}

#[test]
fn dock_close_request_unmaps_and_keeps_sibling() {
    let mut f = two_windows();
    // The runtime tick answers a reported shell close with the polite
    // client close; the client honors it by going away (drop here
    // stands in for a real client exit, exactly like the Alt+F4 test).
    assert!(f.manager.close_window(f.id_a));
    pump(&mut f.comp, &mut f.queue_a, &mut f.client_a, |c| {
        c.close_requested
    });
    assert!(f.client_a.close_requested);
    drop((f.conn_a, f.queue_a, f.client_a));
    f.comp.pump();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.model().window(f.id_a).is_none());
    assert!(f.manager.model().window(f.id_b).is_some());
}

#[test]
fn disconnect_unmaps_window_and_falls_back_focus() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    drop(f.conn_a);
    drop(f.queue_a);
    drop(f.client_a);
    // Let the server observe the hangup before reconciling.
    f.comp.pump();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);

    let remaining: Vec<_> = f.manager.model().windows().collect();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, f.id_b);
    assert!(f.manager.geometry(f.id_a).is_none());
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
}

#[test]
fn overview_parks_keyboard_focus_and_restores_on_dismiss() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));

    // Third client: the shell overview layer surface.
    let (conn_c, mut queue_c, mut client_c) = connect(&mut f.comp);
    let qh_c = queue_c.handle();
    let surface_c = client_c
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh_c, ());
    let layer_c = client_c.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface_c,
        None,
        Layer::Top,
        OVERVIEW_NAMESPACE.to_owned(),
        &qh_c,
        (),
    );
    layer_c.set_size(0, 200);
    layer_c.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
    layer_c.set_exclusive_zone(0);
    surface_c.commit();
    client_c.overview_layer = Some(layer_c);
    client_c.synced = false;
    conn_c.display().sync(&qh_c, ());
    pump(&mut f.comp, &mut queue_c, &mut client_c, |c| c.synced);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.comp.state.overview_surface().is_some());
    assert!(client_c.overview_layer.is_some());
    assert!(!f.manager.overview_focus_held());

    // Opening the overview parks focus: the model unfocuses and keys
    // reach the overview surface owner, not the windows.
    f.manager.set_overview_open(true);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.overview_focus_held());
    assert_eq!(f.manager.model().focused(), None);
    f.client_a.keys.clear();
    f.client_b.keys.clear();
    client_c.keys.clear();
    press(&mut f.manager, &mut f.comp, KEY_A);
    release(&mut f.manager, &mut f.comp, KEY_A);
    for _ in 0..3 {
        drain3(
            &mut f.comp,
            &mut f.queue_a,
            &mut f.client_a,
            &mut f.queue_b,
            &mut f.client_b,
            &mut queue_c,
            &mut client_c,
        );
    }
    assert!(f.client_a.keys.is_empty(), "parked keys leaked to A");
    assert!(f.client_b.keys.is_empty(), "parked keys leaked to B");
    assert_eq!(
        client_c.keys,
        vec![(KEY_A, true), (KEY_A, false)],
        "parked keys must reach the overview owner"
    );

    // Dismiss restores the pre-overview window and delivery with it.
    f.manager.set_overview_open(false);
    f.manager.reconcile(&mut f.comp.state);
    assert!(!f.manager.overview_focus_held());
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    press(&mut f.manager, &mut f.comp, KEY_A);
    release(&mut f.manager, &mut f.comp, KEY_A);
    for _ in 0..3 {
        drain3(
            &mut f.comp,
            &mut f.queue_a,
            &mut f.client_a,
            &mut f.queue_b,
            &mut f.client_b,
            &mut queue_c,
            &mut client_c,
        );
    }
    assert_eq!(
        f.client_a.keys,
        vec![(KEY_A, true), (KEY_A, false)],
        "restored keys must reach A again"
    );
    assert_eq!(client_c.keys.len(), 2, "dismissed keys must not reach C");

    // A vanished overview surface restores early too, so keys never
    // route into a dead surface.
    f.manager.set_overview_open(true);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.overview_focus_held());
    drop((conn_c, queue_c, client_c));
    f.comp.pump();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert!(!f.manager.overview_focus_held());
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
}

#[test]
fn fresh_session_defaults_to_gnome_mode() {
    let f = two_windows();
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Floating)
    );
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
}

#[test]
fn super_shift_t_toggle_round_trip_restores_floating() {
    let mut f = layout_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_a.keys.clear();
    f.client_b.keys.clear();

    let before_a = f.manager.geometry(f.id_a).unwrap();
    let before_b = f.manager.geometry(f.id_b).unwrap();
    let mut before_ids: Vec<u64> = f.manager.model().windows().map(|w| w.id).collect();
    before_ids.sort();

    // Super+Shift+T flips floating into the strip. The T press is
    // consumed like the other Super chords; the release still reaches
    // the client so modifiers never stick.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, T_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Scroll);
    assert_eq!(f.manager.window_layout(f.id_a), Some(WindowLayout::Strip));
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    release(&mut f.manager, &mut f.comp, T_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    for (name, client) in [("alpha", &f.client_a), ("beta", &f.client_b)] {
        assert!(
            !client
                .keys
                .iter()
                .any(|(key, pressed)| *key == T_KEYCODE && *pressed),
            "{name} observed a consumed T press: {:?}",
            client.keys
        );
    }
    assert!(
        f.client_b
            .keys
            .iter()
            .any(|(key, pressed)| *key == T_KEYCODE && !pressed),
        "T release must still reach the focused client, got: {:?}",
        f.client_b.keys
    );

    // Toggling back restores the exact floating geometry with no
    // window lost.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, T_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    release(&mut f.manager, &mut f.comp, T_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    let mut after_ids: Vec<u64> = f.manager.model().windows().map(|w| w.id).collect();
    after_ids.sort();
    assert_eq!(after_ids, before_ids, "toggle round trip lost a window");
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Floating)
    );
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), before_a);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), before_b);
}

#[test]
fn strip_geometry_gaps_overflow_and_advertise() {
    let mut f = layout_windows();
    // Third column: the strip overflows past the right edge instead of
    // squeezing to fit.
    let (conn_c, mut queue_c, mut client_c) = connect(&mut f.comp);
    map_toplevel(&mut f.comp, &conn_c, &mut queue_c, &mut client_c, "gamma");
    f.manager.reconcile(&mut f.comp.state);
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    sync_client(&mut f.comp, &conn_c, &mut queue_c, &mut client_c);
    let id_c = f
        .manager
        .model()
        .windows()
        .find(|w| w.title == "gamma")
        .unwrap()
        .id;
    let floating: Vec<_> = [f.id_a, f.id_b, id_c]
        .into_iter()
        .map(|id| (id, f.manager.geometry(id).unwrap()))
        .collect();

    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert_eq!(f.manager.session_mode(), SessionMode::Scroll);

    // Work area is 1280x768 (800 minus the 32px panel); the default
    // column is (1280 - 16) * 0.5 - 16 = 616 wide, a 16px gap above and
    // below (736 tall from y 48), stepped by 616 + 16 = 632 per column
    // from a 16px start gap. The view follows the focused column
    // (gamma, mapped last) to the strip's end: offset 632.
    let mut xs: Vec<i32> = [f.id_a, f.id_b, id_c]
        .into_iter()
        .map(|id| {
            let geo = f.manager.geometry(id).unwrap();
            assert_eq!((geo.loc.y, geo.size.w, geo.size.h), (48, 616, 736));
            assert_eq!(f.manager.window_layout(id), Some(WindowLayout::Strip));
            geo.loc.x
        })
        .collect();
    xs.sort();
    assert_eq!(xs, vec![-616, 16, 648]);
    assert_eq!(f.manager.strip_offset(), 632.0);

    // The strip size is advertised through configure, tiled on every
    // edge.
    pump(&mut f.comp, &mut f.queue_a, &mut f.client_a, |c| {
        c.configure_sizes.last() == Some(&(616, 736))
    });
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.configure_sizes.last() == Some(&(616, 736))
    });
    pump(&mut f.comp, &mut queue_c, &mut client_c, |c| {
        c.configure_sizes.last() == Some(&(616, 736))
    });
    for (name, states) in [
        ("alpha", f.client_a.configure_states.last()),
        ("beta", f.client_b.configure_states.last()),
        ("gamma", client_c.configure_states.last()),
    ] {
        let states = states.expect("strip configure must advertise state");
        assert!(
            !has_state(states, 1) && !has_state(states, 2),
            "{name} advertised maximized or fullscreen in strip mode: {states:?}"
        );
        // niri's columns are tiled on every edge (left, right, top,
        // bottom): CSD drops its shadow and rounded corners.
        assert!(
            [5, 6, 7, 8]
                .into_iter()
                .all(|state| has_state(states, state)),
            "{name} is not tiled on every edge in strip mode: {states:?}"
        );
    }

    // Leaving the strip restores every floating geometry exactly.
    assert!(f.manager.set_scroll(&mut f.comp.state, false));
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    for (id, geo) in &floating {
        assert_eq!(f.manager.window_layout(*id), Some(WindowLayout::Floating));
        assert_eq!(f.manager.geometry(*id).unwrap(), *geo);
    }
}

#[test]
fn super_t_without_shift_does_not_toggle() {
    let mut f = layout_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();

    // Bare Super+T is ordinary input: forwarded to the client (which is
    // also what disarms the Super-tap), and the session stays gnome.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, T_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    release(&mut f.manager, &mut f.comp, T_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert_eq!(
        f.client_b
            .keys
            .iter()
            .filter(|(key, _)| *key == T_KEYCODE)
            .copied()
            .collect::<Vec<_>>(),
        vec![(T_KEYCODE, true), (T_KEYCODE, false)],
        "unshifted T must reach the client whole, got: {:?}",
        f.client_b.keys
    );
}

/// Three mapped windows ("alpha", "beta", "gamma") on a 1280x800
/// output with a reconciled manager, for strip chord tests.
struct ThreeFixture {
    comp: TestCompositor,
    manager: WindowManager,
    conn_a: Connection,
    queue_a: EventQueue<Client>,
    client_a: Client,
    conn_b: Connection,
    queue_b: EventQueue<Client>,
    client_b: Client,
    conn_c: Connection,
    queue_c: EventQueue<Client>,
    client_c: Client,
    id_a: u64,
    id_b: u64,
    id_c: u64,
}

fn three_windows() -> ThreeFixture {
    let mut f = layout_windows();
    let (conn_c, mut queue_c, mut client_c) = connect(&mut f.comp);
    map_toplevel(&mut f.comp, &conn_c, &mut queue_c, &mut client_c, "gamma");
    f.manager.reconcile(&mut f.comp.state);
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    sync_client(&mut f.comp, &conn_c, &mut queue_c, &mut client_c);
    let id_c = f
        .manager
        .model()
        .windows()
        .find(|w| w.title == "gamma")
        .unwrap()
        .id;
    let Fixture {
        comp,
        manager,
        conn_a,
        queue_a,
        client_a,
        conn_b,
        queue_b,
        client_b,
        id_a,
        id_b,
    } = f;
    ThreeFixture {
        comp,
        manager,
        conn_a,
        queue_a,
        client_a,
        conn_b,
        queue_b,
        client_b,
        conn_c,
        queue_c,
        client_c,
        id_a,
        id_b,
        id_c,
    }
}

fn sync_three(f: &mut ThreeFixture) {
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    sync_client(&mut f.comp, &f.conn_c, &mut f.queue_c, &mut f.client_c);
}

fn clear_three(f: &mut ThreeFixture) {
    f.client_a.keys.clear();
    f.client_b.keys.clear();
    f.client_c.keys.clear();
}

/// Column x positions of alpha/beta/gamma along the strip (screen x
/// plus the view offset, less the 16px start gap): the order focus
/// motion must never disturb, whatever the view follows.
fn column_xs(f: &ThreeFixture) -> (i32, i32, i32) {
    let along = |id| f.manager.geometry(id).unwrap().loc.x + f.manager.strip_offset() as i32 - 16;
    (along(f.id_a), along(f.id_b), along(f.id_c))
}

/// Column x positions of alpha/beta/gamma on screen: where the view
/// puts them.
fn screen_xs(f: &ThreeFixture) -> (i32, i32, i32) {
    (
        f.manager.geometry(f.id_a).unwrap().loc.x,
        f.manager.geometry(f.id_b).unwrap().loc.x,
        f.manager.geometry(f.id_c).unwrap().loc.x,
    )
}

/// No arrow press reached any client: chord presses are consumed and
/// only releases (plus bare modifiers) are observed.
fn assert_no_arrow_press(f: &ThreeFixture) {
    for (name, client) in [
        ("alpha", &f.client_a),
        ("beta", &f.client_b),
        ("gamma", &f.client_c),
    ] {
        assert!(
            !client.keys.iter().any(|(key, pressed)| [
                ARROW_UP_KEYCODE,
                ARROW_DOWN_KEYCODE,
                ARROW_LEFT_KEYCODE,
                ARROW_RIGHT_KEYCODE
            ]
            .contains(key)
                && *pressed),
            "{name} observed a consumed arrow press: {:?}",
            client.keys
        );
    }
}

#[test]
fn scroll_arrow_focus_moves_and_clamps_without_reordering() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert_eq!(f.manager.session_mode(), SessionMode::Scroll);
    // Columns step 632px: alpha 0, beta 632, gamma 1264.
    assert_eq!(column_xs(&f), (0, 632, 1264));
    // Focusing in scroll mode never reorders the strip, so starting
    // from the leftmost column keeps the order above.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    sync_three(&mut f);
    clear_three(&mut f);

    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    release(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_c));
    release(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    // Clamped at the end: a third step right stays put.
    press(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_c));
    release(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    // Focus motion never reorders columns.
    assert_eq!(column_xs(&f), (0, 632, 1264));

    sync_three(&mut f);
    assert_no_arrow_press(&f);
    // Releases still reach clients so modifiers never stick.
    for keycode in [ARROW_RIGHT_KEYCODE, ARROW_LEFT_KEYCODE] {
        assert!(
            [&f.client_a, &f.client_b, &f.client_c]
                .iter()
                .any(|c| c.keys.contains(&(keycode, false))),
            "arrow release {keycode} must still reach a client",
        );
    }
}

#[test]
fn scroll_shift_arrow_moves_focused_slot() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_three(&mut f);
    clear_three(&mut f);

    // Super+Shift+Right swaps beta one slot right: gamma takes 632,
    // beta takes 1264, and focus follows beta.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    assert_eq!(column_xs(&f), (0, 1264, 632));
    release(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    for id in [f.id_a, f.id_b, f.id_c] {
        let geo = f.manager.geometry(id).unwrap();
        assert_eq!((geo.loc.y, geo.size.w, geo.size.h), (48, 616, 736));
        assert_eq!(f.manager.window_layout(id), Some(WindowLayout::Strip));
    }

    // Super+Shift+Left reverses the swap exactly.
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    assert_eq!(column_xs(&f), (0, 632, 1264));
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);

    // End-clamped moves are no-ops keeping geometry.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    assert_eq!(column_xs(&f), (0, 632, 1264));
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_c)));
    press(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    assert_eq!(f.manager.model().focused(), Some(f.id_c));
    assert_eq!(column_xs(&f), (0, 632, 1264));
    release(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    sync_three(&mut f);
    assert_no_arrow_press(&f);
}

#[test]
fn gnome_shift_arrows_still_tile_and_consume() {
    let mut f = layout_windows();
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();

    // Shift is ignored by the gnome arrow chords (as today): a
    // shifted Left tiles left exactly like the bare chord.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Tiled(TileSide::Left))
    );
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    // Back to floating, then the bare chord tiles left too.
    press(&mut f.manager, &mut f.comp, ARROW_DOWN_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
    release(&mut f.manager, &mut f.comp, ARROW_DOWN_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Tiled(TileSide::Left))
    );
    release(&mut f.manager, &mut f.comp, ARROW_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);

    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert!(
        !f.client_b.keys.iter().any(|(key, pressed)| [
            ARROW_UP_KEYCODE,
            ARROW_DOWN_KEYCODE,
            ARROW_LEFT_KEYCODE,
            ARROW_RIGHT_KEYCODE
        ]
        .contains(key)
            && *pressed),
        "arrow presses must be consumed, got: {:?}",
        f.client_b.keys
    );
}

#[test]
fn scroll_up_down_stay_columns_in_scroll() {
    let mut f = layout_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();
    f.client_b.configure_sizes.clear();
    f.client_b.configure_states.clear();

    // Forced membership: Super+Up mid-scroll joins as a same-height
    // column instead of covering the screen.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_UP_KEYCODE);
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    let col = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((col.loc.x, col.loc.y), (648, 48));
    assert_eq!((col.size.w, col.size.h), (616, 736));
    release(&mut f.manager, &mut f.comp, ARROW_UP_KEYCODE);
    // Super+Down afterwards keeps the column: no floating restore
    // mid-strip.
    press(&mut f.manager, &mut f.comp, ARROW_DOWN_KEYCODE);
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), col);
    release(&mut f.manager, &mut f.comp, ARROW_DOWN_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    // The column size is advertised with no managed xdg state.
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.configure_sizes.last() == Some(&(616, 736))
    });
    let states = f
        .client_b
        .configure_states
        .last()
        .expect("strip configure must advertise state");
    assert!(
        !has_state(states, 1) && !has_state(states, 2),
        "scroll column advertised a managed state: {states:?}"
    );
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert!(
        !f.client_b
            .keys
            .iter()
            .any(|(key, pressed)| [ARROW_UP_KEYCODE, ARROW_DOWN_KEYCODE].contains(key) && *pressed),
        "arrow presses must be consumed, got: {:?}",
        f.client_b.keys
    );
}

/// Drive one wheel tick straight into the manager (no backend): the
/// axis unit is whatever the backend reported (v120 or pixels).
fn axis(manager: &mut WindowManager, comp: &mut TestCompositor, horizontal: f64, vertical: f64) {
    manager.on_input(
        &mut comp.state,
        ManagerInput::Axis {
            horizontal,
            vertical,
            time: 6000,
        },
    );
}

#[test]
fn scroll_axis_moves_strip_view_and_reports_offset() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    // Focusing alpha brings the view back to the strip's start.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    assert_eq!(screen_xs(&f), (16, 648, 1280));
    assert_eq!(f.manager.strip_offset(), 0.0);

    // One vertical tick shifts every column left; the getter tracks
    // the accumulated offset.
    axis(&mut f.manager, &mut f.comp, 0.0, 120.0);
    assert_eq!(f.manager.strip_offset(), 120.0);
    assert_eq!(screen_xs(&f), (-104, 528, 1160));
    // Horizontal and vertical amounts accumulate into one offset.
    axis(&mut f.manager, &mut f.comp, 30.0, 0.0);
    assert_eq!(f.manager.strip_offset(), 150.0);
    assert_eq!(screen_xs(&f), (-134, 498, 1130));
    // Scrolling never moves focus.
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
}

#[test]
fn scroll_axis_clamps_at_overflow_bounds() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    // Three 616-wide columns with 16px gaps between and at both ends
    // total 1912px on a 1280px work area: at most 632px of scroll.
    axis(&mut f.manager, &mut f.comp, 0.0, 100_000.0);
    assert_eq!(f.manager.strip_offset(), 632.0);
    assert_eq!(screen_xs(&f), (-616, 16, 648));
    // Past the end further ticks are no-ops keeping geometry.
    axis(&mut f.manager, &mut f.comp, 0.0, 120.0);
    assert_eq!(f.manager.strip_offset(), 632.0);
    assert_eq!(screen_xs(&f), (-616, 16, 648));
    // A huge pull back clamps at the start.
    axis(&mut f.manager, &mut f.comp, 0.0, -100_000.0);
    assert_eq!(f.manager.strip_offset(), 0.0);
    assert_eq!(screen_xs(&f), (16, 648, 1280));
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
}

#[test]
fn strip_view_springs_to_target_while_geometry_jumps() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    // Entering the strip lays it out in place: nothing to animate.
    assert_eq!(f.manager.strip_view(), f.manager.strip_offset());
    assert!(!f.manager.strip_animating());
    // Start from the strip's end, settled.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_c)));
    while f.manager.step_strip_view(1.0 / 60.0) {}
    assert_eq!(f.manager.strip_offset(), 632.0);
    assert_eq!(f.manager.strip_view(), 632.0);

    // Focusing alpha moves the target at once: geometry, configure and
    // the reported offset are all at the target.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    assert_eq!(f.manager.strip_offset(), 0.0);
    assert_eq!(screen_xs(&f), (16, 648, 1280));
    // Only the drawn view trails, still where it was.
    assert_eq!(f.manager.strip_view(), 632.0);
    assert!(f.manager.strip_animating());
    assert_eq!(f.manager.render_geometry(f.id_a).unwrap().loc.x, -616);

    // 60 Hz frames: the view glides monotonically onto the target and
    // settles in niri's time; geometry never moves meanwhile.
    let mut last = f.manager.strip_view();
    let mut frames = 0;
    while f.manager.step_strip_view(1.0 / 60.0) {
        frames += 1;
        let view = f.manager.strip_view();
        assert!(view <= last && view >= 0.0, "view {view} after {last}");
        last = view;
        assert_eq!(screen_xs(&f), (16, 648, 1280));
        let drawn = f.manager.render_geometry(f.id_a).unwrap().loc.x;
        assert_eq!(drawn, 16 - view.round() as i32);
        assert!(frames < 60, "never settled");
    }
    // ~0.42 s at 60 Hz, a little past niri's 0.33 s envelope estimate.
    assert!((18..=28).contains(&frames), "settled after {frames} frames");
    assert_eq!(f.manager.strip_view(), 0.0);
    assert!(!f.manager.strip_animating());
    assert_eq!(
        f.manager.render_geometry(f.id_a),
        f.manager.geometry(f.id_a)
    );

    // Retargeting mid-flight continues from where the view is drawn,
    // with no jump.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_c)));
    for _ in 0..5 {
        f.manager.step_strip_view(1.0 / 60.0);
    }
    let mid = f.manager.strip_view();
    assert!(mid > 0.0 && mid < 632.0);
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    assert_eq!(f.manager.strip_offset(), 0.0);
    f.manager.step_strip_view(1.0 / 60.0);
    assert_eq!(f.manager.strip_view(), mid);
    f.manager.step_strip_view(1.0 / 60.0);
    // Still carrying its rightward velocity a moment, then turning.
    assert!((f.manager.strip_view() - mid).abs() < 80.0);
    while f.manager.step_strip_view(1.0 / 60.0) {}
    assert_eq!(f.manager.strip_view(), 0.0);

    // Wheel scrolling is direct: the view follows without a spring.
    axis(&mut f.manager, &mut f.comp, 0.0, 120.0);
    assert_eq!(f.manager.strip_view(), 120.0);
    assert!(!f.manager.step_strip_view(1.0 / 60.0));
}

#[test]
fn scroll_axis_ignored_without_overflow() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let (conn_a, mut queue_a, mut client_a) = connect(&mut comp);
    map_toplevel(&mut comp, &conn_a, &mut queue_a, &mut client_a, "solo");
    comp.state.set_output_size(1280, 800);
    manager.reconcile(&mut comp.state);
    sync_client(&mut comp, &conn_a, &mut queue_a, &mut client_a);
    let id = manager
        .model()
        .windows()
        .find(|w| w.title == "solo")
        .unwrap()
        .id;
    assert!(manager.set_scroll(&mut comp.state, true));
    let before = manager.geometry(id).unwrap();
    // A single 616-wide column fits the 1280px work area: nothing to
    // scroll, so the offset stays put and geometry is untouched.
    axis(&mut manager, &mut comp, 0.0, 120.0);
    assert_eq!(manager.strip_offset(), 0.0);
    assert_eq!(manager.geometry(id).unwrap(), before);
}

#[test]
fn scroll_axis_keeps_focused_window() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    // The view sits at the end (it followed gamma); beta is whole.
    assert_eq!(f.manager.strip_offset(), 632.0);
    for _ in 0..5 {
        axis(&mut f.manager, &mut f.comp, 0.0, -120.0);
        assert_eq!(f.manager.model().focused(), Some(f.id_b));
    }
    assert_eq!(f.manager.strip_offset(), 32.0);
    for _ in 0..5 {
        axis(&mut f.manager, &mut f.comp, 0.0, 120.0);
        assert_eq!(f.manager.model().focused(), Some(f.id_b));
    }
    assert_eq!(f.manager.strip_offset(), 632.0);
}

#[test]
fn scroll_axis_ignored_in_gnome_mode() {
    let mut f = three_windows();
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    let before_a = f.manager.geometry(f.id_a).unwrap();
    let before_b = f.manager.geometry(f.id_b).unwrap();
    let before_c = f.manager.geometry(f.id_c).unwrap();
    axis(&mut f.manager, &mut f.comp, 0.0, 120.0);
    axis(&mut f.manager, &mut f.comp, 120.0, 0.0);
    assert_eq!(f.manager.strip_offset(), 0.0);
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), before_a);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), before_b);
    assert_eq!(f.manager.geometry(f.id_c).unwrap(), before_c);
}

#[test]
fn reentering_scroll_resets_strip_offset() {
    let mut f = three_windows();
    // Entering follows the focused column (gamma, at the end).
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert_eq!(f.manager.strip_offset(), 632.0);
    axis(&mut f.manager, &mut f.comp, 0.0, -120.0);
    assert_eq!(f.manager.strip_offset(), 512.0);
    assert_eq!(screen_xs(&f), (-496, 136, 768));
    // Re-entering forgets the scroll and follows focus afresh.
    f.manager.set_scroll(&mut f.comp.state, false);
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert_eq!(f.manager.strip_offset(), 632.0);
    assert_eq!(screen_xs(&f), (-616, 16, 648));
}

#[test]
fn scroll_maximize_joins_strip_as_column() {
    let mut f = layout_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.configure_sizes.clear();
    f.client_b.configure_states.clear();

    // Maximizing mid-scroll joins as a same-height column: Strip
    // layout, full work height, 16px-gap column width — never the
    // 1280x768 work area.
    assert!(f.manager.set_maximized(&mut f.comp.state, f.id_b, true));
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    let col = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((col.loc.x, col.loc.y), (648, 48));
    assert_eq!((col.size.w, col.size.h), (616, 736));
    // Un-maximizing mid-scroll stays a column too.
    assert!(f.manager.set_maximized(&mut f.comp.state, f.id_b, false));
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), col);
    assert!(!f.manager.set_maximized(&mut f.comp.state, 999, true));

    // The column size is advertised with no Maximized state.
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.configure_sizes.last() == Some(&(616, 736))
    });
    let states = f
        .client_b
        .configure_states
        .last()
        .expect("strip configure must advertise state");
    assert!(
        !has_state(states, 1),
        "scroll column advertised Maximized: {states:?}"
    );
}

#[test]
fn scroll_fullscreen_joins_strip_as_column() {
    let mut f = layout_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.configure_sizes.clear();
    f.client_b.configure_states.clear();

    // Fullscreen mid-scroll joins as a same-height column instead of
    // covering the 1280x800 output.
    assert!(f.manager.set_fullscreen(&mut f.comp.state, f.id_b, true));
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    let col = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((col.loc.x, col.loc.y), (648, 48));
    assert_eq!((col.size.w, col.size.h), (616, 736));
    // Un-fullscreen mid-scroll stays a column too.
    assert!(f.manager.set_fullscreen(&mut f.comp.state, f.id_b, false));
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), col);
    assert!(!f.manager.set_fullscreen(&mut f.comp.state, 999, true));

    // The column size is advertised with no Fullscreen state.
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        c.configure_sizes.last() == Some(&(616, 736))
    });
    let states = f
        .client_b
        .configure_states
        .last()
        .expect("strip configure must advertise state");
    assert!(
        !has_state(states, 2),
        "scroll column advertised Fullscreen: {states:?}"
    );
}

#[test]
fn scroll_tile_reresolves_to_column() {
    let mut f = layout_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));

    // Tiling mid-scroll is already a column, so both sides re-resolve
    // to the strip instead of splitting the work area into halves.
    assert!(f
        .manager
        .set_tiled(&mut f.comp.state, f.id_b, TileSide::Left));
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    let col = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((col.loc.x, col.loc.y), (648, 48));
    assert_eq!((col.size.w, col.size.h), (616, 736));
    assert!(f
        .manager
        .set_tiled(&mut f.comp.state, f.id_b, TileSide::Right));
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), col);
    assert!(!f.manager.set_tiled(&mut f.comp.state, 999, TileSide::Left));
}

#[test]
fn scroll_map_lands_directly_in_strip() {
    let mut f = layout_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);

    // Mapping mid-scroll lands directly in the strip: the third
    // window extends the column sequence instead of cascading.
    let (conn_c, mut queue_c, mut client_c) = connect(&mut f.comp);
    map_toplevel(&mut f.comp, &conn_c, &mut queue_c, &mut client_c, "gamma");
    f.manager.reconcile(&mut f.comp.state);
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    sync_client(&mut f.comp, &conn_c, &mut queue_c, &mut client_c);
    let id_c = f
        .manager
        .model()
        .windows()
        .find(|w| w.title == "gamma")
        .unwrap()
        .id;
    for id in [f.id_a, f.id_b, id_c] {
        assert_eq!(f.manager.window_layout(id), Some(WindowLayout::Strip));
        let geo = f.manager.geometry(id).unwrap();
        assert_eq!((geo.loc.y, geo.size.w, geo.size.h), (48, 616, 736));
    }
    let mut xs: Vec<i32> = [f.id_a, f.id_b, id_c]
        .into_iter()
        .map(|id| f.manager.geometry(id).unwrap().loc.x)
        .collect();
    xs.sort();
    assert_eq!(xs, vec![-616, 16, 648]);

    // The strip size is advertised with no managed xdg state.
    pump(&mut f.comp, &mut queue_c, &mut client_c, |c| {
        c.configure_sizes.last() == Some(&(616, 736))
    });
    let states = client_c
        .configure_states
        .last()
        .expect("strip configure must advertise state");
    assert!(
        !has_state(states, 1) && !has_state(states, 2),
        "mapped strip column advertised a managed state: {states:?}"
    );
}

#[test]
fn scroll_client_requests_join_as_columns() {
    let mut f = layout_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.configure_sizes.clear();
    f.client_b.configure_states.clear();

    // Client maximize/unmaximize/fullscreen requests drain through the
    // same setters, so every direction resolves to a column.
    f.client_b.toplevel.as_ref().unwrap().set_maximized();
    f.queue_b.flush().unwrap();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    let col = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((col.loc.x, col.loc.y), (648, 48));
    assert_eq!((col.size.w, col.size.h), (616, 736));

    f.client_b.toplevel.as_ref().unwrap().unset_maximized();
    f.queue_b.flush().unwrap();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), col);

    f.client_b
        .toplevel
        .as_ref()
        .unwrap()
        .set_fullscreen(None::<&wayland_client::protocol::wl_output::WlOutput>);
    f.queue_b.flush().unwrap();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(f.manager.window_layout(f.id_b), Some(WindowLayout::Strip));
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), col);

    // The column size is advertised with no managed xdg state.
    pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
        !c.configure_sizes.is_empty()
    });
    for states in &f.client_b.configure_states {
        assert!(
            !has_state(states, 1) && !has_state(states, 2),
            "scroll column advertised a managed state: {states:?}"
        );
    }
}

#[test]
fn gnome_maximized_toggle_scroll_restores_floating_exact() {
    let mut f = layout_windows();
    let before_a = f.manager.geometry(f.id_a).unwrap();
    let before_b = f.manager.geometry(f.id_b).unwrap();

    // Maximizing in gnome stashes the floating geometry; toggling to
    // scroll keeps that stash alive underneath the strip column.
    assert!(f.manager.set_maximized(&mut f.comp.state, f.id_a, true));
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Maximized)
    );
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert_eq!(f.manager.session_mode(), SessionMode::Scroll);
    assert_eq!(f.manager.window_layout(f.id_a), Some(WindowLayout::Strip));
    let col = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((col.loc.x, col.loc.y), (16, 48));
    assert_eq!((col.size.w, col.size.h), (616, 736));

    // Toggling back restores the exact floating geometry stashed
    // before the gnome maximize.
    assert!(f.manager.set_scroll(&mut f.comp.state, false));
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Floating)
    );
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), before_a);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), before_b);
}

#[test]
fn gnome_client_fullscreen_request_applies_on_reconcile() {
    let mut f = layout_windows();
    f.client_b
        .toplevel
        .as_ref()
        .unwrap()
        .set_fullscreen(None::<&wayland_client::protocol::wl_output::WlOutput>);
    f.queue_b.flush().unwrap();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Fullscreen)
    );
    let full = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((full.loc.x, full.loc.y), (0, 0));
    assert_eq!((full.size.w, full.size.h), (1280, 800));
    // And back off again through the protocol.
    f.client_b.toplevel.as_ref().unwrap().unset_fullscreen();
    f.queue_b.flush().unwrap();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
}

/// No R press reached any client: preset-cycle presses are consumed
/// and only releases (plus bare modifiers) are observed.
fn assert_no_r_press(f: &ThreeFixture) {
    for (name, client) in [
        ("alpha", &f.client_a),
        ("beta", &f.client_b),
        ("gamma", &f.client_c),
    ] {
        assert!(
            !client
                .keys
                .iter()
                .any(|(key, pressed)| *key == R_KEYCODE && *pressed),
            "{name} observed a consumed R press: {:?}",
            client.keys
        );
    }
    assert!(
        [&f.client_a, &f.client_b, &f.client_c]
            .iter()
            .any(|c| c.keys.contains(&(R_KEYCODE, false))),
        "R release must still reach a client",
    );
}

#[test]
fn scroll_preset_forward_cycles_and_wraps() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    sync_three(&mut f);
    clear_three(&mut f);
    // Preset widths on the 1280px work area via the gaps-twice
    // formula (((1280 - 16) * p) as i32 - 16): 1/3 -> 405,
    // 1/2 -> 616, 2/3 -> 826.
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 616);

    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    // 1/2 -> 2/3: the focused column widens and later columns shift
    // right (beta 826 + 16 = 842, gamma 842 + 616 + 16 = 1474).
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 826);
    assert_eq!(column_xs(&f), (0, 842, 1474));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    // 2/3 -> 1/3: beta 405 + 16 = 421, gamma 421 + 616 + 16 = 1053.
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 405);
    assert_eq!(column_xs(&f), (0, 421, 1053));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    // 1/3 -> 1/2 wraps back to the default strip geometry.
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 616);
    assert_eq!(column_xs(&f), (0, 632, 1264));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    // Full work height throughout the cycle.
    for id in [f.id_a, f.id_b, f.id_c] {
        let geo = f.manager.geometry(id).unwrap();
        assert_eq!((geo.loc.y, geo.size.h), (48, 736));
        assert_eq!(f.manager.window_layout(id), Some(WindowLayout::Strip));
    }
    sync_three(&mut f);
    assert_no_r_press(&f);
}

#[test]
fn scroll_preset_backward_cycles() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    sync_three(&mut f);
    clear_three(&mut f);

    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    // 1/2 -> 1/3 backward from the default.
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 405);
    assert_eq!(column_xs(&f), (0, 421, 1053));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    // 1/3 -> 2/3 wraps backward past the start.
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 826);
    assert_eq!(column_xs(&f), (0, 842, 1474));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    // 2/3 -> 1/2 returns to the default.
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 616);
    assert_eq!(column_xs(&f), (0, 632, 1264));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    sync_three(&mut f);
    assert_no_r_press(&f);
}

#[test]
fn scroll_preset_cycle_keeps_other_columns() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_three(&mut f);
    clear_three(&mut f);

    // Give beta a 2/3 preset first: alpha 616 at 0, beta 826 at 632,
    // gamma 616 at 632 + 826 + 16 = 1474.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_b).unwrap().size.w, 826);
    assert_eq!(column_xs(&f), (0, 632, 1474));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    // Cycling alpha leaves beta's preset (and gamma's default) alone:
    // alpha 826 at 0, beta 826 at 842, gamma 616 at 842 + 826 + 16.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 826);
    assert_eq!(f.manager.geometry(f.id_b).unwrap().size.w, 826);
    assert_eq!(f.manager.geometry(f.id_c).unwrap().size.w, 616);
    assert_eq!(column_xs(&f), (0, 842, 1684));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    sync_three(&mut f);
    assert_no_r_press(&f);
}

#[test]
fn gnome_super_r_forwards_to_client() {
    let mut f = layout_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_b.keys.clear();
    let before_a = f.manager.geometry(f.id_a).unwrap();
    let before_b = f.manager.geometry(f.id_b).unwrap();

    // No R binding in gnome mode: the press forwards untouched and
    // nothing re-lays out.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    assert_eq!(
        f.manager.window_layout(f.id_b),
        Some(WindowLayout::Floating)
    );
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), before_a);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), before_b);
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert_eq!(
        f.client_b
            .keys
            .iter()
            .filter(|(key, _)| *key == R_KEYCODE)
            .copied()
            .collect::<Vec<_>>(),
        vec![(R_KEYCODE, true), (R_KEYCODE, false)],
        "gnome R must reach the client whole, got: {:?}",
        f.client_b.keys
    );
}

#[test]
fn scroll_preset_survives_toggle_and_restores_floating() {
    let mut f = layout_windows();
    let before_a = f.manager.geometry(f.id_a).unwrap();
    let before_b = f.manager.geometry(f.id_b).unwrap();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_a.keys.clear();

    // Cycle the focused column to 2/3 through the real chord.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 826);
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    // Toggling back restores the exact floating geometry cycled
    // columns stashed on entry.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, T_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Gnome);
    release(&mut f.manager, &mut f.comp, T_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Floating)
    );
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), before_a);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), before_b);

    // Re-entering scroll remembers the preset choice: alpha is still
    // 2/3 wide with beta at 16 + 826 + 16 = 858.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, T_KEYCODE);
    assert_eq!(f.manager.session_mode(), SessionMode::Scroll);
    release(&mut f.manager, &mut f.comp, T_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().size.w, 826);
    assert_eq!(f.manager.geometry(f.id_b).unwrap().size.w, 616);
    assert_eq!(
        (
            f.manager.geometry(f.id_a).unwrap().loc.x,
            f.manager.geometry(f.id_b).unwrap().loc.x
        ),
        (16, 858)
    );
}

#[test]
fn new_columns_open_right_of_focus_and_the_view_follows_like_niri() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    assert_eq!(f.manager.strip_offset(), 0.0);

    // A window mapped mid-strip opens right of the focused column (niri),
    // takes focus, and shows whole without the view moving.
    let (conn_d, mut queue_d, mut client_d) = connect(&mut f.comp);
    map_toplevel(&mut f.comp, &conn_d, &mut queue_d, &mut client_d, "delta");
    f.manager.reconcile(&mut f.comp.state);
    let id_d = f
        .manager
        .model()
        .windows()
        .find(|w| w.title == "delta")
        .unwrap()
        .id;
    assert_eq!(f.manager.model().focused(), Some(id_d));
    let along = |f: &ThreeFixture, id| {
        f.manager.geometry(id).unwrap().loc.x + f.manager.strip_offset() as i32 - 16
    };
    assert_eq!(
        [f.id_a, id_d, f.id_b, f.id_c].map(|id| along(&f, id)),
        [0, 632, 1264, 1896]
    );
    assert_eq!(f.manager.strip_offset(), 0.0);
    assert_eq!(f.manager.geometry(id_d).unwrap().loc.x, 648);

    // Focusing the far column scrolls the least that shows it whole: it
    // ends a gap from the right edge.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_c)));
    assert_eq!(f.manager.strip_offset(), 1264.0);
    let gamma = f.manager.geometry(f.id_c).unwrap();
    assert_eq!(gamma.loc.x + gamma.size.w, 1280 - 16);
    // And back to the first: it starts a gap from the left edge.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    assert_eq!(f.manager.strip_offset(), 0.0);
    assert_eq!(f.manager.geometry(f.id_a).unwrap().loc.x, 16);
}

#[test]
fn scroll_preset_shrink_reclamps_offset() {
    let mut f = three_windows();
    assert!(f.manager.set_scroll(&mut f.comp.state, true));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_c)));
    // Three 616-wide columns with 16px gaps between and at both ends
    // total 1912px on a 1280px work area: the view sits at the 632px
    // max, following gamma.
    axis(&mut f.manager, &mut f.comp, 0.0, 100_000.0);
    assert_eq!(f.manager.strip_offset(), 632.0);
    assert_eq!(screen_xs(&f), (-616, 16, 648));
    sync_three(&mut f);
    clear_three(&mut f);

    // Cycling the focused last column narrower (1/2 -> 1/3 = 405)
    // shrinks the strip to 616 + 616 + 405 + 4 * 16 = 1701px, so the
    // max scroll drops to 421px and the stranded offset re-clamps:
    // alpha 16 - 421, beta 648 - 421 = 227, gamma 1280 - 421.
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.geometry(f.id_c).unwrap().size.w, 405);
    assert_eq!(f.manager.strip_offset(), 421.0);
    assert_eq!(screen_xs(&f), (-405, 227, 859));
    assert_eq!(f.manager.model().focused(), Some(f.id_c));
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, SHIFT_LEFT_KEYCODE);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);

    // No blank tail: the last column ends a gap from the work edge.
    let gamma = f.manager.geometry(f.id_c).unwrap();
    assert_eq!((gamma.loc.y, gamma.size.h), (48, 736));
    assert_eq!(gamma.loc.x + gamma.size.w, 1280 - 16);
    sync_three(&mut f);
    assert_no_r_press(&f);
}

/// Unified handles: every managed entry holds the toolkit's `Window`
/// on its native variant, one distinct live surface per client.
#[test]
fn managed_entries_hold_unified_window_handles() {
    let f = two_windows();
    let visible = f.manager.visible_windows();
    assert_eq!(visible.len(), 2);
    for (window, _geometry) in &visible {
        assert!(window.is_wayland());
        assert!(window.alive());
        assert!(window.wl_surface().is_some());
        assert!(window.toplevel().is_some());
        assert!(matches!(
            window.underlying_surface(),
            WindowSurface::Wayland(_)
        ));
    }
    // One distinct underlying surface per entry: the handles track
    // alpha's and beta's own surfaces, not one shared handle.
    let ids: Vec<_> = visible
        .iter()
        .map(|(window, _)| window.toplevel().unwrap().wl_surface().id())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

/// One reconcile path end to end for native entries through the
/// unified type: reconcile maps, focus targets, polite close asks,
/// and the next reconcile after client exit drops the entry with
/// focus falling back to the sibling.
#[test]
fn unified_round_trip_reconcile_focus_close_reconcile() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    assert_eq!(f.manager.model().focused(), Some(f.id_a));

    // Polite close through the unified handle: the client observes
    // the request, but the live entry stays managed until it exits.
    assert!(f.manager.close_window(f.id_a));
    pump(&mut f.comp, &mut f.queue_a, &mut f.client_a, |c| {
        c.close_requested
    });
    assert!(f.client_a.close_requested);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.model().window(f.id_a).is_some());

    // Client exit: the alive sweep drops the entry on the next
    // reconcile and focus falls back to beta.
    drop((f.conn_a, f.queue_a, f.client_a));
    f.comp.pump();
    f.comp.pump();
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.model().window(f.id_a).is_none());
    assert!(f.manager.geometry(f.id_a).is_none());
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    assert_eq!(f.manager.visible_windows().len(), 1);
    assert!(!f.manager.close_window(f.id_a));
}

#[test]
fn picking_a_window_in_the_overview_survives_the_close() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));

    // Third client: the shell overview layer surface.
    let (conn_c, mut queue_c, mut client_c) = connect(&mut f.comp);
    let qh_c = queue_c.handle();
    let surface_c = client_c
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh_c, ());
    let layer_c = client_c.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface_c,
        None,
        Layer::Top,
        OVERVIEW_NAMESPACE.to_owned(),
        &qh_c,
        (),
    );
    layer_c.set_size(0, 200);
    layer_c.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
    layer_c.set_exclusive_zone(0);
    surface_c.commit();
    client_c.overview_layer = Some(layer_c);
    client_c.synced = false;
    conn_c.display().sync(&qh_c, ());
    pump(&mut f.comp, &mut queue_c, &mut client_c, |c| c.synced);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.comp.state.overview_surface().is_some());
    assert!(client_c.overview_layer.is_some());
    assert!(!f.manager.overview_focus_held());

    // Opening the overview parks focus: the model unfocuses and keys
    // reach the overview surface owner, not the windows.
    f.manager.set_overview_open(true);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.overview_focus_held());
    assert_eq!(f.manager.model().focused(), None);
    // A preview pick (or dash/search activation) while parked: the
    // chosen window, not the pre-overview one, is focused on close.
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    assert_eq!(f.manager.model().focused(), None, "still parked while open");
    f.manager.set_overview_open(false);
    f.manager.reconcile(&mut f.comp.state);
    assert!(!f.manager.overview_focus_held());
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    drop((conn_c, queue_c, client_c));
}

#[test]
fn a_window_launched_from_the_overview_is_focused_when_it_closes() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));

    // Third client: the shell overview layer surface.
    let (conn_c, mut queue_c, mut client_c) = connect(&mut f.comp);
    let qh_c = queue_c.handle();
    let surface_c = client_c
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh_c, ());
    let layer_c = client_c.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface_c,
        None,
        Layer::Top,
        OVERVIEW_NAMESPACE.to_owned(),
        &qh_c,
        (),
    );
    layer_c.set_size(0, 200);
    layer_c.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
    layer_c.set_exclusive_zone(0);
    surface_c.commit();
    client_c.overview_layer = Some(layer_c);
    client_c.synced = false;
    conn_c.display().sync(&qh_c, ());
    pump(&mut f.comp, &mut queue_c, &mut client_c, |c| c.synced);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.comp.state.overview_surface().is_some());
    assert!(client_c.overview_layer.is_some());
    assert!(!f.manager.overview_focus_held());

    f.manager.set_overview_open(true);
    f.manager.reconcile(&mut f.comp.state);
    assert!(f.manager.overview_focus_held());

    // An app launched from search maps while the overview still holds
    // focus (the launch outruns the close): it, not the pre-overview
    // window, is focused once the overview closes (GNOME shape).
    let (conn_d, mut queue_d, mut client_d) = connect(&mut f.comp);
    map_toplevel(
        &mut f.comp,
        &conn_d,
        &mut queue_d,
        &mut client_d,
        "launched",
    );
    f.manager.reconcile(&mut f.comp.state);
    let launched = f
        .manager
        .model()
        .windows()
        .find(|w| w.title == "launched")
        .map(|w| w.id)
        .expect("launched window mapped");
    f.manager.set_overview_open(false);
    f.manager.reconcile(&mut f.comp.state);
    assert_eq!(f.manager.model().focused(), Some(launched));
    drop((conn_c, queue_c, client_c, conn_d, queue_d, client_d));
}

#[test]
fn shell_activation_deactivates_the_previous_window() {
    // The shell's ActivateWindow sets the model's focus first, then the
    // runtime applies it: the window that had focus must still hear it
    // lost activation (one activated window, as under Mutter).
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_b)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert!(f
        .client_b
        .configure_states
        .last()
        .is_some_and(|s| has_state(s, 4)));
    let _ = f.manager.model_mut().set_focused(Some(f.id_a));
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert!(
        f.client_a
            .configure_states
            .last()
            .is_some_and(|s| has_state(s, 4)),
        "the activated window hears it"
    );
    assert!(
        f.client_b
            .configure_states
            .last()
            .is_some_and(|s| !has_state(s, 4)),
        "the previous window is deactivated"
    );
}

#[test]
fn strip_columns_resize_on_primary_output_failover_without_losing_focus() {
    let mut f = layout_windows();
    f.comp.state.add_output("secondary", None, 1024, 768);
    f.manager.set_scroll(&mut f.comp.state, true);
    let focused = f.manager.model().focused();
    let workspace = f.manager.model().active_workspace();
    assert!(f.comp.state.remove_output("roost-0"));
    f.manager.reapply_derived_layouts(&mut f.comp.state);
    for id in [f.id_a, f.id_b] {
        let geometry = f.manager.geometry(id).unwrap();
        assert_eq!((geometry.size.w, geometry.size.h), (488, 704));
        assert_eq!(f.manager.window_layout(id), Some(WindowLayout::Strip));
    }
    assert_eq!(f.manager.model().focused(), focused);
    assert_eq!(f.manager.model().active_workspace(), workspace);
}

#[test]
fn exclusive_overlay_receives_navigation_and_retains_popup_system_accelerators() {
    use roost_shell_control::{Accelerator, MODE_NORMAL, MODE_POPUP, MOD_ALT, MOD_CTRL};
    let mut f = two_windows();
    let (conn, mut queue, mut client) = connect(&mut f.comp);
    let qh = queue.handle();
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer = client.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        Layer::Overlay,
        "roost-screenshot-ui".into(),
        &qh,
        (),
    );
    layer.set_size(0, 0);
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();
    client.synced = false;
    conn.display().sync(&qh, ());
    pump(&mut f.comp, &mut queue, &mut client, |c| c.synced);
    // Configure acknowledgements are sent while dispatching the first sync.
    // Commit that acknowledged size before asking the compositor for focus.
    surface.commit();
    client.synced = false;
    conn.display().sync(&qh, ());
    pump(&mut f.comp, &mut queue, &mut client, |c| c.synced);
    f.manager.set_overview_open(true);
    f.manager.reconcile(&mut f.comp.state);
    assert!(roost_compositor::layer::exclusive_keyboard_layer(&f.comp.state).is_some());
    f.manager.set_accelerators(vec![
        Accelerator {
            action: 1,
            keysym: 0xff53,
            mods: MOD_CTRL | MOD_ALT,
            modes: MODE_NORMAL,
        },
        Accelerator {
            action: 2,
            keysym: u32::from(b'r'),
            mods: 0,
            modes: MODE_NORMAL | MODE_POPUP,
        },
    ]);
    client.keys.clear();
    f.client_a.keys.clear();
    f.client_b.keys.clear();
    let workspace = f.manager.model().active_workspace();
    for key in [ALT_LEFT_KEYCODE, CTRL_LEFT_KEYCODE, ARROW_RIGHT_KEYCODE] {
        press(&mut f.manager, &mut f.comp, key);
    }
    for key in [ARROW_RIGHT_KEYCODE, CTRL_LEFT_KEYCODE, ALT_LEFT_KEYCODE] {
        release(&mut f.manager, &mut f.comp, key);
    }
    assert!(f.manager.take_accelerators_fired().is_empty());
    assert_eq!(f.manager.model().active_workspace(), workspace);
    press(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    release(&mut f.manager, &mut f.comp, TAB_KEYCODE);
    release(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    assert!(f.manager.take_switcher_queue().is_empty());
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(f.manager.take_accelerators_fired(), [(2, 5000, MODE_POPUP)]);
    for _ in 0..3 {
        drain3(
            &mut f.comp,
            &mut f.queue_a,
            &mut f.client_a,
            &mut f.queue_b,
            &mut f.client_b,
            &mut queue,
            &mut client,
        );
    }
    assert!(client.keys.contains(&(ARROW_RIGHT_KEYCODE, true)));
    assert!(client.keys.contains(&(ARROW_RIGHT_KEYCODE, false)));
    assert!(client.keys.contains(&(TAB_KEYCODE, true)));
    assert!(client.keys.contains(&(TAB_KEYCODE, false)));
    assert!(!client.keys.iter().any(|(key, _)| *key == R_KEYCODE));
    assert!(f.client_a.keys.is_empty());
    assert!(f.client_b.keys.is_empty());
    f.manager.set_overview_open(false);
    // Unmapping returns normal desktop accelerators to their original mode.
    layer.destroy();
    surface.destroy();
    client.synced = false;
    conn.display().sync(&qh, ());
    pump(&mut f.comp, &mut queue, &mut client, |c| c.synced);
    f.manager.reconcile(&mut f.comp.state);
    press(&mut f.manager, &mut f.comp, ALT_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, CTRL_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, ARROW_RIGHT_KEYCODE);
    assert_eq!(
        f.manager.take_accelerators_fired(),
        [(1, 5000, MODE_NORMAL)]
    );
}

#[test]
fn exclusive_overview_keeps_overview_accelerators() {
    use roost_shell_control::{Accelerator, MODE_OVERVIEW, MOD_LOGO, OVERVIEW_NAMESPACE};
    let mut f = two_windows();
    let (conn, mut queue, mut client) = connect(&mut f.comp);
    let qh = queue.handle();
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer = client.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        Layer::Overlay,
        OVERVIEW_NAMESPACE.into(),
        &qh,
        (),
    );
    layer.set_size(0, 0);
    layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    for _ in 0..2 {
        surface.commit();
        client.synced = false;
        conn.display().sync(&qh, ());
        pump(&mut f.comp, &mut queue, &mut client, |c| c.synced);
    }
    f.manager.set_overview_open(true);
    f.manager.reconcile(&mut f.comp.state);
    assert!(roost_compositor::layer::exclusive_keyboard_layer(&f.comp.state).is_some());
    assert!(roost_compositor::layer::exclusive_popup_keyboard_layer(&f.comp.state).is_none());
    f.manager.set_accelerators(vec![Accelerator {
        action: 17,
        keysym: u32::from(b'a'),
        mods: MOD_LOGO,
        modes: MODE_OVERVIEW,
    }]);
    press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    press(&mut f.manager, &mut f.comp, 30);
    release(&mut f.manager, &mut f.comp, 30);
    release(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
    assert_eq!(
        f.manager.take_accelerators_fired(),
        [(17, 5000, MODE_OVERVIEW)]
    );
}

#[test]
fn modifier_releases_during_lock_do_not_turn_unlocked_clicks_into_moves() {
    for mapped_lock in [true, false] {
        let mut f = two_windows();
        press(&mut f.manager, &mut f.comp, SUPER_LEFT_KEYCODE);
        let release = ManagerInput::Key {
            keycode: SUPER_LEFT_KEYCODE,
            pressed: false,
            time: 5001,
        };
        if mapped_lock {
            let surface = f.manager.surface_of(f.id_a).unwrap();
            f.manager.lock_input(&mut f.comp.state, &surface, release);
        } else {
            // The runtime observes releases even before a lock surface maps;
            // no client receives this otherwise-consumed event.
            f.manager.discard_key_input(&mut f.comp.state, &release);
        }
        assert!(f.comp.state.pressed_key_count() == 0);
        f.manager
            .pointer_motion(&mut f.comp.state, beta_only(), 5002);
        f.manager
            .pointer_button(&mut f.comp.state, BTN_LEFT, true, 5003);
        f.manager
            .pointer_button(&mut f.comp.state, BTN_LEFT, false, 5004);
        pump(&mut f.comp, &mut f.queue_b, &mut f.client_b, |c| {
            c.pointer_buttons.len() >= 2
        });
        assert_eq!(
            f.client_b.pointer_buttons,
            vec![(BTN_LEFT, true), (BTN_LEFT, false)]
        );
        assert_eq!(f.manager.model().focused(), Some(f.id_b));
    }
}

#[test]
fn swallowed_accelerator_release_clears_physical_pressed_state() {
    use roost_shell_control::{Accelerator, MODE_NORMAL};
    let mut f = two_windows();
    f.manager.set_accelerators(vec![Accelerator {
        action: 23,
        keysym: u32::from(b'r'),
        mods: 0,
        modes: MODE_NORMAL,
    }]);
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert_eq!(
        f.manager.take_accelerators_fired(),
        [(23, 5000, MODE_NORMAL)]
    );
    assert_eq!(f.comp.state.pressed_key_count(), 1);
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    assert!(f.comp.state.pressed_key_count() == 0);
    assert!(f.manager.take_accelerators_fired().is_empty());
}

#[test]
fn same_focused_window_click_restores_keyboard_after_lock_ownership() {
    let mut f = two_windows();
    assert!(f.manager.focus(&mut f.comp.state, Some(f.id_a)));
    let lock_surface = f.manager.surface_of(f.id_b).unwrap();
    f.manager.lock_input(
        &mut f.comp.state,
        &lock_surface,
        ManagerInput::Motion {
            pos: beta_only(),
            time: 5001,
        },
    );
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    f.client_a.keys.clear();
    f.client_b.keys.clear();
    f.manager
        .pointer_motion(&mut f.comp.state, alpha_only(), 5002);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 5003);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 5004);
    press(&mut f.manager, &mut f.comp, R_KEYCODE);
    release(&mut f.manager, &mut f.comp, R_KEYCODE);
    sync_client(&mut f.comp, &f.conn_a, &mut f.queue_a, &mut f.client_a);
    sync_client(&mut f.comp, &f.conn_b, &mut f.queue_b, &mut f.client_b);
    assert_eq!(f.client_a.keys, [(R_KEYCODE, true), (R_KEYCODE, false)]);
    assert!(
        f.client_b.keys.is_empty(),
        "the former lock owner hears no unlocked keys"
    );
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
}
