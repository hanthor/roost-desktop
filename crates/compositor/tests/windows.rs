//! T2 window mapping, input routing, and focus-policy tests.
//!
//! Headless end-to-end coverage for `rwd_compositor::windows` with no
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

use rwd_compositor::windows::{
    ManagerInput, TileSide, WindowLayout, WindowManager, ALT_LEFT_KEYCODE, ARROW_DOWN_KEYCODE,
    ARROW_LEFT_KEYCODE, ARROW_RIGHT_KEYCODE, ARROW_UP_KEYCODE, ESCAPE_KEYCODE, F4_KEYCODE,
    PAGE_DOWN_KEYCODE, PAGE_UP_KEYCODE, SHIFT_LEFT_KEYCODE, SUPER_LEFT_KEYCODE, TAB_KEYCODE,
};
use rwd_compositor::TestCompositor;
use rwd_shell_control::SwitcherAction;
use smithay::utils::{Logical, Point};
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
/// (32,32), both 800x600.
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

/// A point inside alpha (0,0-800,600) but outside beta (32,32-832,632).
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
    assert_eq!(locs, vec![(0, 0), (32, 32)]);

    // Mapping focuses: exactly one window holds keyboard focus.
    let focused = f.manager.model().focused().unwrap();
    assert!(focused == f.id_a || focused == f.id_b);
    assert_eq!(f.manager.visible_windows().len(), 2);
}

#[test]
fn pointer_motion_moves_focus_to_window_under_cursor() {
    let mut f = two_windows();
    f.manager
        .pointer_motion(&mut f.comp.state, beta_only(), 1000);
    assert_eq!(f.manager.model().focused(), Some(f.id_b));
    f.manager
        .pointer_motion(&mut f.comp.state, alpha_only(), 1001);
    assert_eq!(f.manager.model().focused(), Some(f.id_a));
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
    assert_eq!((moved.loc.x, moved.loc.y), (82, 92));
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
    // Beta sits cascaded at (32,32); the dialog centers on it, which a
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
    assert_eq!((geo.loc.x, geo.loc.y), (32, 32));
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
