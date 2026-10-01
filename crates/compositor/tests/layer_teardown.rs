//! Hiding a layer surface the way GTK does must not kill the client.
//!
//! gtk4-layer-shell hides a window by destroying its
//! zwlr_layer_surface_v1 and then committing a null buffer on the same
//! wl_surface. Smithay 0.7 validated that commit against reset layer
//! state and posted a protocol error, disconnecting the client: the GTK
//! shell died every time the overview closed. The workaround in
//! `State::new_surface` keeps that sequence legal; this test drives it
//! with a real client and asserts the connection survives.

use std::os::unix::net::UnixStream;

use roost_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_callback::WlCallback, wl_compositor::WlCompositor, wl_display::WlDisplay,
        wl_registry::WlRegistry, wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{Anchor, Event as LayerEvent, ZwlrLayerSurfaceV1},
};

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    configured: bool,
    synced: bool,
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
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
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()))
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: <ZwlrLayerSurfaceV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let LayerEvent::Configure { serial, .. } = event {
            layer.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        _: <WlCallback as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}

macro_rules! empty {
    ($t:ty) => {
        impl Dispatch<$t, ()> for Client {
            fn event(
                _: &mut Self,
                _: &$t,
                _: <$t as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}
empty!(WlDisplay);
empty!(WlCompositor);
empty!(WlSurface);
empty!(ZwlrLayerShellV1);

fn pump(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
    done: impl Fn(&Client) -> bool,
) -> Result<(), String> {
    for _ in 0..200 {
        queue.flush().map_err(|e| e.to_string())?;
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            guard.read().map_err(|e| e.to_string())?;
        }
        queue.dispatch_pending(client).map_err(|e| e.to_string())?;
        if done(client) {
            return Ok(());
        }
    }
    Err("pump budget exhausted".into())
}

#[test]
fn destroying_a_layer_role_then_committing_keeps_the_client_connected() {
    let mut comp = TestCompositor::new();
    let _manager = comp.window_manager();
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    pump(&mut comp, &mut queue, &mut client, |c| {
        c.compositor.is_some() && c.layer_shell.is_some()
    })
    .unwrap();

    // A top-anchored, explicitly sized surface (the GTK search popover
    // shape): legal while the role lives.
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer = client.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        Layer::Top,
        "roost-shell-overview".into(),
        &qh,
        (),
    );
    layer.set_anchor(Anchor::Top);
    layer.set_size(370, 48);
    surface.commit();
    pump(&mut comp, &mut queue, &mut client, |c| c.configured).unwrap();

    // GTK's hide: destroy the role, then commit a null buffer.
    layer.destroy();
    surface.attach(None, 0, 0);
    surface.commit();

    client.synced = false;
    conn.display().sync(&qh, ());
    let result = pump(&mut comp, &mut queue, &mut client, |c| c.synced);
    assert!(
        result.is_ok(),
        "client was disconnected after hiding its layer surface: {result:?}"
    );
    assert!(
        conn.protocol_error().is_none(),
        "{:?}",
        conn.protocol_error()
    );
}
