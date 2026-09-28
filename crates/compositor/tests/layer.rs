//! T4 layer-shell panel bring-up tests.
//!
//! Protocol-level proof that the compositor serves `zwlr_layer_shell_v1`
//! for the supervised panel: a raw client attaches a top-anchored layer
//! surface with an exclusive zone, the server records it and sends the
//! initial configure, ack marks it configured, server close reaches the
//! client, and destroy clears the record. No fallback surface roles
//! exist anywhere in this flow.
//!
//! Conventions follow `windows.rs`: socketpair clients, a bounded pump
//! budget, plain asserts, no sleeps. All code here is original.

use std::os::unix::net::UnixStream;

use rwd_compositor::TestCompositor;
use smithay::wayland::{
    compositor::with_states,
    shell::wlr_layer::{
        Anchor, ExclusiveZone, KeyboardInteractivity, Layer, LayerSurfaceCachedState,
    },
};
use wayland_client::{
    protocol::{
        wl_callback::WlCallback, wl_compositor::WlCompositor, wl_output::WlOutput,
        wl_registry::WlRegistry, wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer as ClientLayer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{
        Anchor as ClientAnchor, Event as LayerEvent, KeyboardInteractivity as ClientKeyboard,
        ZwlrLayerSurfaceV1,
    },
};

const PUMP_ROUNDS: usize = 200;
const PANEL_NAMESPACE: &str = "rwd-shell-panel";
const PANEL_HEIGHT: u32 = 32;

/// One panel-shaped protocol client.
#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    surface: Option<WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    synced: bool,
    configured: bool,
    last_serial: u32,
    closed: bool,
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some() && self.layer_shell.is_some()
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

empty_dispatch!(WlCompositor);
empty_dispatch!(WlSurface);
empty_dispatch!(WlOutput);
empty_dispatch!(ZwlrLayerShellV1);

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

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            LayerEvent::Configure { serial, .. } => {
                proxy.ack_configure(serial);
                state.configured = true;
                state.last_serial = serial;
                if let Some(surface) = &state.surface {
                    surface.commit();
                }
            }
            LayerEvent::Closed => {
                state.closed = true;
            }
            _ => {}
        }
    }
}

/// Pump the server and drain one client queue until `done` or the round
/// budget runs out. The predicate sees both sides after each round.
fn pump(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
    done: impl Fn(&TestCompositor, &Client) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            guard.read().unwrap();
        }
        queue.dispatch_pending(client).unwrap();
        if done(comp, client) {
            return;
        }
    }
    panic!("pump budget exhausted");
}

/// Connect one client and bind compositor plus layer shell.
fn connect(comp: &mut TestCompositor) -> (Connection, EventQueue<Client>, Client) {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server_stream);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    pump(comp, &mut queue, &mut client, |_, c| c.ready());
    (conn, queue, client)
}

/// Attach the panel-shaped layer surface: top-anchored full-width strip
/// with an exclusive zone, mirroring `ShellHost::create_panel_surface`.
fn attach_panel(queue: &EventQueue<Client>, client: &mut Client) {
    let qh = queue.handle();
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer_surface = client.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        ClientLayer::Top,
        PANEL_NAMESPACE.to_owned(),
        &qh,
        (),
    );
    layer_surface.set_size(0, PANEL_HEIGHT);
    layer_surface.set_anchor(ClientAnchor::Top | ClientAnchor::Left | ClientAnchor::Right);
    layer_surface.set_exclusive_zone(PANEL_HEIGHT as i32);
    layer_surface.set_keyboard_interactivity(ClientKeyboard::OnDemand);
    surface.commit();
    client.surface = Some(surface);
    client.layer_surface = Some(layer_surface);
}

#[test]
fn panel_attaches_top_with_exclusive_zone_and_configures() {
    let mut comp = TestCompositor::new();
    let (_conn, mut queue, mut client) = connect(&mut comp);
    attach_panel(&queue, &mut client);

    // The server records the panel with its namespace and layer, and the
    // client receives the initial configure.
    pump(&mut comp, &mut queue, &mut client, |comp, c| {
        c.configured && comp.state.panel_surfaces().len() == 1
    });
    let panels = comp.state.panel_surfaces();
    assert_eq!(panels.len(), 1);
    assert_eq!(panels[0].namespace, PANEL_NAMESPACE);
    assert_eq!(panels[0].layer, Layer::Top);

    // The client's ack plus commit marks the panel configured, and the
    // committed zone/anchor match the panel contract.
    pump(&mut comp, &mut queue, &mut client, |comp, _| {
        comp.state
            .panel_surfaces()
            .first()
            .is_some_and(|panel| panel.configured)
    });
    let servers = comp.state.layer_surfaces();
    assert_eq!(servers.len(), 1);
    let (zone, anchor, keyboard) = with_states(servers[0].wl_surface(), |states| {
        let current = *states
            .cached_state
            .get::<LayerSurfaceCachedState>()
            .current();
        (
            current.exclusive_zone,
            current.anchor,
            current.keyboard_interactivity,
        )
    });
    assert_eq!(zone, ExclusiveZone::Exclusive(PANEL_HEIGHT));
    assert!(anchor.contains(Anchor::TOP), "panel anchors to top");
    assert_eq!(keyboard, KeyboardInteractivity::OnDemand);
}

#[test]
fn server_close_reaches_panel() {
    let mut comp = TestCompositor::new();
    let (_conn, mut queue, mut client) = connect(&mut comp);
    attach_panel(&queue, &mut client);
    pump(&mut comp, &mut queue, &mut client, |comp, c| {
        c.configured && comp.state.panel_surfaces().len() == 1
    });

    comp.state.layer_surfaces()[0].send_close();
    pump(&mut comp, &mut queue, &mut client, |_, c| c.closed);
    assert!(client.closed, "panel observes server close");
}

#[test]
fn destroyed_panel_leaves_no_record() {
    let mut comp = TestCompositor::new();
    let (_conn, mut queue, mut client) = connect(&mut comp);
    attach_panel(&queue, &mut client);
    pump(&mut comp, &mut queue, &mut client, |comp, c| {
        c.configured && comp.state.panel_surfaces().len() == 1
    });

    client.layer_surface.as_ref().unwrap().destroy();
    client.surface.as_ref().unwrap().destroy();
    client.layer_surface = None;
    client.surface = None;
    pump(&mut comp, &mut queue, &mut client, |comp, _| {
        comp.state.panel_surfaces().is_empty()
    });
    assert!(comp.state.panel_surfaces().is_empty());
}
