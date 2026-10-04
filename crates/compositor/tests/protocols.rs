//! Advertised protocols (#89) and the xdg-activation focus policy.
//!
//! The globals list is golden: adding or dropping a protocol is a
//! deliberate change that updates `EXPECTED` and `docs/protocols.md`
//! together. xdg-decoration stays absent on purpose (Mutter parity).

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;

use roost_compositor::windows::WindowManager;
use roost_compositor::TestCompositor;
use smithay::backend::allocator::{Fourcc, Modifier};
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::{
    activation::v1::client::{
        xdg_activation_token_v1::{self, XdgActivationTokenV1},
        xdg_activation_v1::XdgActivationV1,
    },
    shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
};

const ROUNDS: usize = 200;

/// Every global a client sees on a headless compositor, by version.
/// linux-dmabuf joins once a renderer reports formats (see below), and
/// wl_output once an output is connected.
const EXPECTED: &[(&str, u32)] = &[
    ("ext_session_lock_manager_v1", 1),
    ("wl_compositor", 6),
    ("wl_data_device_manager", 3),
    ("wl_seat", 9),
    ("wl_shm", 2),
    ("wl_subcompositor", 1),
    ("wp_commit_timing_manager_v1", 1),
    ("wp_cursor_shape_manager_v1", 2),
    ("wp_fifo_manager_v1", 1),
    ("wp_fractional_scale_manager_v1", 1),
    ("wp_pointer_warp_v1", 1),
    ("wp_presentation", 2),
    ("wp_single_pixel_buffer_manager_v1", 1),
    ("wp_viewporter", 1),
    ("xdg_activation_v1", 1),
    ("xdg_system_bell_v1", 1),
    ("xdg_toplevel_tag_manager_v1", 1),
    ("xdg_wm_base", 6),
    ("xdg_wm_dialog_v1", 1),
    ("zwlr_layer_shell_v1", 4),
    ("zwp_idle_inhibit_manager_v1", 1),
    ("zwp_input_method_manager_v2", 1),
    ("zwp_keyboard_shortcuts_inhibit_manager_v1", 1),
    ("zwp_pointer_constraints_v1", 1),
    ("zwp_pointer_gestures_v1", 3),
    ("zwp_primary_selection_device_manager_v1", 1),
    ("zwp_relative_pointer_manager_v1", 1),
    ("zwp_text_input_manager_v3", 1),
    ("zxdg_exporter_v2", 1),
    ("zxdg_importer_v2", 1),
    ("zxdg_output_manager_v1", 3),
];

#[derive(Default)]
struct Client {
    globals: BTreeMap<String, (u32, u32)>,
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    wm: Option<XdgWmBase>,
    activation: Option<XdgActivationV1>,
    keyboard_serial: Option<u32>,
    token: Option<String>,
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()))
                }
                "wl_seat" => state.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "xdg_wm_base" => state.wm = Some(registry.bind(name, 1, qh, ())),
                "xdg_activation_v1" => state.activation = Some(registry.bind(name, 1, qh, ())),
                _ => {}
            }
            state.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<WlSeat, ()> for Client {
    fn event(
        _: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: wayland_client::WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { serial, .. } = event {
            state.keyboard_serial = Some(serial);
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            state.token = Some(token);
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore XdgActivationV1);

struct Peer {
    _conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
}

fn connect(comp: &mut TestCompositor) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    conn.display().get_registry(&queue.handle(), ());
    Peer {
        _conn: conn,
        queue,
        client: Client::default(),
    }
}

fn pump(comp: &mut TestCompositor, manager: &mut WindowManager, peers: &mut [&mut Peer]) {
    for _ in 0..ROUNDS / 20 {
        for p in peers.iter_mut() {
            p.queue.flush().unwrap();
        }
        comp.pump();
        manager.reconcile(&mut comp.state);
        comp.pump();
        for p in peers.iter_mut() {
            if let Some(guard) = p.queue.prepare_read() {
                let _ = guard.read();
            }
            p.queue.dispatch_pending(&mut p.client).unwrap();
        }
    }
}

/// Map one toplevel; returns its surface.
fn window(peer: &mut Peer, title: &str) -> WlSurface {
    let qh = peer.queue.handle();
    let surface = peer
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = peer
        .client
        .wm
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(title.into());
    surface.commit();
    // The protocol objects live with the connection.
    std::mem::forget((xdg, toplevel));
    surface
}

fn focused_title(manager: &WindowManager) -> Option<String> {
    let id = manager.model().focused()?;
    manager
        .model()
        .windows()
        .find(|w| w.id == id)
        .map(|w| w.title.clone())
}

#[test]
fn advertised_globals_match_the_golden_list() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let seen: Vec<(String, u32)> = peer
        .client
        .globals
        .iter()
        .map(|(k, (_, v))| (k.clone(), *v))
        .collect();
    let expected: Vec<(String, u32)> = EXPECTED
        .iter()
        .map(|(k, v)| ((*k).to_owned(), *v))
        .collect();
    assert_eq!(
        seen, expected,
        "update EXPECTED and docs/protocols.md together"
    );
    assert!(
        !peer
            .client
            .globals
            .contains_key("zxdg_decoration_manager_v1"),
        "GNOME is client-side decorations only"
    );
}

#[test]
fn dmabuf_is_advertised_only_with_renderer_formats() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    comp.state.enable_dmabuf(Vec::new());
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(!peer.client.globals.contains_key("zwp_linux_dmabuf_v1"));

    comp.state
        .enable_dmabuf(vec![smithay::backend::allocator::Format {
            code: Fourcc::Argb8888,
            modifier: Modifier::Linear,
        }]);
    let mut late = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut late]);
    assert_eq!(
        late.client.globals.get("zwp_linux_dmabuf_v1").map(|g| g.1),
        Some(3)
    );
}

fn mint(peer: &mut Peer, with_serial: bool) -> XdgActivationTokenV1 {
    let qh = peer.queue.handle();
    let token = peer
        .client
        .activation
        .as_ref()
        .unwrap()
        .get_activation_token(&qh, ());
    if with_serial {
        let serial = peer.client.keyboard_serial.expect("keyboard enter");
        token.set_serial(serial, peer.client.seat.as_ref().unwrap());
    }
    token.commit();
    token
}

#[test]
fn focused_client_can_hand_focus_with_a_token() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut peer = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let first = window(&mut peer, "first");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _second = window(&mut peer, "second");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("second"));

    let _token = mint(&mut peer, true);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let token = peer.client.token.take().expect("token issued");
    peer.client
        .activation
        .as_ref()
        .unwrap()
        .activate(token.clone(), &first);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("first"));

    // One use: replaying the token after focus moves changes nothing.
    manager.focus(
        &mut comp.state,
        manager.model().windows().map(|w| w.id).max(),
    );
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    peer.client
        .activation
        .as_ref()
        .unwrap()
        .activate(token, &first);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("second"));
}

#[test]
fn background_clients_and_serial_less_tokens_cannot_steal_focus() {
    let mut comp = TestCompositor::new();
    let mut manager = comp.window_manager();
    let mut back = connect(&mut comp);
    let mut front = connect(&mut comp);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let back_window = window(&mut back, "back");
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let _front_window = window(&mut front, "front");
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    assert_eq!(focused_title(&manager).as_deref(), Some("front"));

    // The background client still has its old enter serial, but it no
    // longer holds keyboard focus.
    let _t = mint(&mut back, true);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let token = back
        .client
        .token
        .take()
        .expect("a token string is always sent");
    back.client
        .activation
        .as_ref()
        .unwrap()
        .activate(token, &back_window);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    assert_eq!(focused_title(&manager).as_deref(), Some("front"));

    // The focused client without an input serial is refused too.
    let _t = mint(&mut front, false);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    let token = front.client.token.take().expect("token string");
    front
        .client
        .activation
        .as_ref()
        .unwrap()
        .activate(token, &back_window);
    pump(&mut comp, &mut manager, &mut [&mut back, &mut front]);
    assert_eq!(focused_title(&manager).as_deref(), Some("front"));
}
