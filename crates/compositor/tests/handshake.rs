//! 001 slice-1 verification: a client connects, maps an shm-backed
//! xdg_toplevel, and the compositor observes both the toplevel and the
//! committed buffer. Runs headless (socketpair, no EGL, no window).

use std::os::unix::{io::AsFd, net::UnixStream};

use rwd_compositor::TestCompositor;
use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
use smithay::wayland::compositor;
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
        wl_display::WlDisplay, wl_registry::WlRegistry, wl_shm::WlShm, wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

const WIDTH: i32 = 64;
const HEIGHT: i32 = 64;
const PUMP_ROUNDS: usize = 200;

struct Client {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    xdg_base: Option<XdgWmBase>,
    surface: Option<WlSurface>,
    xdg_surface: Option<XdgSurface>,
    synced: bool,
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
                "wl_shm" => {
                    state.shm = Some(registry.bind::<WlShm, _, _>(name, version.min(2), qh, ()));
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
empty_dispatch!(WlShm);
empty_dispatch!(WlShmPool);
empty_dispatch!(WlBuffer);
empty_dispatch!(XdgToplevel);

/// Pump the server and drain the client queue until `done` or the round
/// budget runs out. Single-threaded by design: neither side blocks.
fn roundtrip(
    comp: &mut TestCompositor,
    queue: &mut wayland_client::EventQueue<Client>,
    client: &mut Client,
    done: impl Fn(&Client) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
        // Flush client requests so the server sees them, run the server,
        // then read whatever it replied before dispatching locally.
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

#[test]
fn client_maps_shm_toplevel() {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    let mut comp = TestCompositor::new();
    comp.add_client(server_stream);

    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client {
        compositor: None,
        shm: None,
        xdg_base: None,
        surface: None,
        xdg_surface: None,
        synced: false,
    };
    conn.display().get_registry(&qh, ());

    // 1. Globals arrive and bind.
    roundtrip(&mut comp, &mut queue, &mut client, |c| {
        c.compositor.is_some() && c.shm.is_some() && c.xdg_base.is_some()
    });

    // 2. Create an xdg_toplevel and commit (no buffer yet).
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg_surface = client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, ());
    xdg_surface.get_toplevel(&qh, ());
    surface.commit();
    client.surface = Some(surface);
    client.xdg_surface = Some(xdg_surface.clone());
    client.synced = false;
    conn.display().sync(&qh, ());
    roundtrip(&mut comp, &mut queue, &mut client, |c| c.synced);

    // The compositor observes exactly one mapped toplevel.
    assert_eq!(comp.state.toplevel_count(), 1);

    // 3. Attach an shm buffer and commit.
    let stride = WIDTH * 4;
    let pool_size = stride * HEIGHT;
    let file = tempfile::tempfile().unwrap();
    file.set_len(pool_size as u64).unwrap();
    let pool = client
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), pool_size, &qh, ());
    let buffer = pool.create_buffer(
        0,
        WIDTH,
        HEIGHT,
        stride,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        &qh,
        (),
    );
    let surface = client.surface.as_ref().unwrap();
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, WIDTH, HEIGHT);
    surface.commit();
    client.synced = false;
    conn.display().sync(&qh, ());
    roundtrip(&mut comp, &mut queue, &mut client, |c| c.synced);

    // The committed buffer reached the renderer-managed surface state:
    // `commit` hands buffers to `on_commit_buffer_handler`, which consumes
    // them out of `SurfaceAttributes` into `RendererSurfaceState`.
    let wl_surface = comp.state.first_toplevel_surface();
    let has_buffer = compositor::with_states(&wl_surface, |states| {
        states
            .data_map
            .get::<RendererSurfaceStateUserData>()
            .map(|data| data.lock().unwrap().buffer_size().is_some())
            .unwrap_or(false)
    });
    assert!(has_buffer, "committed shm buffer missing server-side");
}
