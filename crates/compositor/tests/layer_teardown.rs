//! Hiding a layer surface the way GTK does must not kill the client.
//!
//! gtk4-layer-shell hides a window by destroying its
//! zwlr_layer_surface_v1 and then committing a null buffer on the same
//! wl_surface. Smithay 0.7 validated that commit against reset layer
//! state and posted a protocol error, disconnecting the client: the GTK
//! shell died every time the overview closed. The workaround in
//! `State::new_surface` keeps that sequence legal; this test drives it
//! with a real client and asserts the connection survives.

use std::os::unix::io::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;

use roost_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
        wl_display::WlDisplay, wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
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
    shm: Option<WlShm>,
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
                "wl_shm" => state.shm = Some(registry.bind(name, version.min(2), qh, ())),
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
empty!(WlShm);
empty!(WlShmPool);
empty!(WlBuffer);

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
        roost_shell_control::OVERVIEW_NAMESPACE.into(),
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

/// How many open fds point at the same file as `file` (the client's own
/// descriptor plus any compositor-side dup received over the socket).
fn backing_count(file: &std::fs::File) -> usize {
    let target =
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).expect("pool fd");
    std::fs::read_dir("/proc/self/fd")
        .expect("fd dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            std::fs::read_link(entry.path())
                .map(|link| link == target)
                .unwrap_or(false)
        })
        .count()
}

/// Poll `backing_count` until it reaches `want` or the deadline passes.
/// Smithay hands freed pools to a background dropping thread (munmap and
/// close can stall the frame loop), so the fd closes asynchronously; a
/// genuine retention never converges.
fn wait_backing_count(file: &std::fs::File, want: usize, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let count = backing_count(file);
        if count == want {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: backing fd count stuck at {count}, want {want}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn roundtrip(
    conn: &Connection,
    qh: &QueueHandle<Client>,
    comp: &mut TestCompositor,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
) {
    client.synced = false;
    conn.display().sync(qh, ());
    pump(comp, queue, client, |c| c.synced).expect("roundtrip");
}

/// A destroyed layer role must not pin its last SHM buffer server-side.
///
/// Notification banners and overview toggles destroy the layer role while
/// the surface keeps its last committed buffer (no null commit follows on
/// that path). The compositor used to stash the whole `WlSurface` in
/// `dead_layer_surfaces`, which kept the buffer — and through it the SHM
/// pool mapping and fd — alive for every short-lived layer surface until
/// the client disconnected (S-GROWTH, #307).
#[test]
fn destroying_a_layer_role_releases_its_shm_backing() {
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
        c.compositor.is_some() && c.layer_shell.is_some() && c.shm.is_some()
    })
    .unwrap();

    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let layer = client.layer_shell.as_ref().unwrap().get_layer_surface(
        &surface,
        None,
        Layer::Top,
        roost_shell_control::GTK_BANNERS_NAMESPACE.into(),
        &qh,
        (),
    );
    layer.set_anchor(Anchor::Top);
    layer.set_size(370, 40);

    let stride = 8 * 4;
    let file = tempfile::tempfile().unwrap();
    file.set_len((stride * 8) as u64).unwrap();
    let pool = client
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), stride * 8, &qh, ());
    client.synced = false;
    conn.display().sync(&qh, ());
    pump(&mut comp, &mut queue, &mut client, |c| c.synced).unwrap();
    // The pool fd crossed the socket: client original plus compositor dup.
    wait_backing_count(&file, 2, "pool dup did not arrive");

    let buffer = pool.create_buffer(
        0,
        8,
        8,
        stride,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    pump(&mut comp, &mut queue, &mut client, |c| c.configured).unwrap();

    // Banner dismissed: role destroyed, surface keeps its last buffer,
    // then the client tears everything down. No null commit in between.
    layer.destroy();
    buffer.destroy();
    pool.destroy();
    surface.destroy();
    roundtrip(&conn, &qh, &mut comp, &mut queue, &mut client);
    roundtrip(&conn, &qh, &mut comp, &mut queue, &mut client);

    assert!(
        conn.protocol_error().is_none(),
        "{:?}",
        conn.protocol_error()
    );
    wait_backing_count(
        &file,
        1,
        "compositor still holds the destroyed layer surface's SHM backing",
    );
}
