//! Proof client with an app id absent from the desktop-file index.
use std::{io::Write, os::fd::AsFd};
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::{
    shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
    toplevel_icon::v1::client::{
        xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1,
        xdg_toplevel_icon_v1::XdgToplevelIconV1,
    },
};
#[derive(Default)]
struct Probe {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm: Option<XdgWmBase>,
    icons: Option<XdgToplevelIconManagerV1>,
    surface: Option<WlSurface>,
    buffer: Option<WlBuffer>,
}
impl Dispatch<WlRegistry, ()> for Probe {
    fn event(
        s: &mut Self,
        r: &WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        q: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = e
        {
            match interface.as_str() {
                "wl_compositor" => s.compositor = Some(r.bind(name, 1, q, ())),
                "wl_shm" => s.shm = Some(r.bind(name, 1, q, ())),
                "xdg_wm_base" => s.wm = Some(r.bind(name, 1, q, ())),
                "xdg_toplevel_icon_manager_v1" => s.icons = Some(r.bind(name, 1, q, ())),
                _ => {}
            }
        }
    }
}
impl Dispatch<XdgWmBase, ()> for Probe {
    fn event(
        _: &mut Self,
        r: &XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            r.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for Probe {
    fn event(
        s: &mut Self,
        r: &XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            r.ack_configure(serial);
            let surface = s.surface.as_ref().unwrap();
            surface.attach(s.buffer.as_ref(), 0, 0);
            surface.damage(0, 0, 240, 160);
            surface.commit();
        }
    }
}
wayland_client::delegate_noop!(Probe: ignore WlCompositor);
wayland_client::delegate_noop!(Probe: ignore WlShm);
wayland_client::delegate_noop!(Probe: ignore WlShmPool);
wayland_client::delegate_noop!(Probe: ignore WlBuffer);
wayland_client::delegate_noop!(Probe: ignore WlSurface);
wayland_client::delegate_noop!(Probe: ignore XdgToplevel);
wayland_client::delegate_noop!(Probe: ignore XdgToplevelIconManagerV1);
wayland_client::delegate_noop!(Probe: ignore XdgToplevelIconV1);
fn buffer(s: &Probe, q: &QueueHandle<Probe>, width: i32, height: i32, icon: bool) -> WlBuffer {
    let mut file = tempfile::tempfile().unwrap();
    for y in 0..height {
        for x in 0..width {
            let color: u32 = if icon {
                if (x + y) % 16 < 8 {
                    0xfff040b0
                } else {
                    0xff20d090
                }
            } else {
                0xff253040
            };
            file.write_all(&color.to_ne_bytes()).unwrap();
        }
    }
    let pool = s
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), width * height * 4, q, ());
    let buffer = pool.create_buffer(0, width, height, width * 4, wl_shm::Format::Argb8888, q, ());
    pool.destroy();
    buffer
}
fn main() {
    #[cfg(feature = "xwayland")]
    if std::env::args().any(|a| a == "--x11-hints") {
        x11_hints();
        return;
    }

    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let q = queue.handle();
    let mut s = Probe::default();
    conn.display().get_registry(&q, ());
    queue.roundtrip(&mut s).unwrap();
    let surface = s.compositor.as_ref().unwrap().create_surface(&q, ());
    let xdg = s.wm.as_ref().unwrap().get_xdg_surface(&surface, &q, ());
    let top = xdg.get_toplevel(&q, ());
    top.set_app_id("roost-window-icon-probe".into());
    top.set_title("Window Icon Probe".into());
    let icons = s.icons.as_ref().unwrap();
    let icon = icons.create_icon(&q, ());
    let pixels = buffer(&s, &q, 32, 32, true);
    icon.add_buffer(&pixels, 1);
    icons.set_icon(&top, Some(&icon));
    icon.destroy();
    s.buffer = Some(buffer(&s, &q, 240, 160, false));
    s.surface = Some(surface.clone());
    surface.commit();
    loop {
        queue.blocking_dispatch(&mut s).unwrap();
    }
}

#[cfg(feature = "xwayland")]
fn x11_hints() {
    use smithay::reexports::x11rb::{
        connection::Connection,
        protocol::xproto::{
            AtomEnum, ConnectionExt, CreateGCAux, CreateWindowAux, PropMode, Rectangle, WindowClass,
        },
        wrapper::ConnectionExt as _,
    };
    let (conn, screen) = smithay::reexports::x11rb::connect(None).unwrap();
    let root = &conn.setup().roots[screen];
    let window = conn.generate_id().unwrap();
    conn.create_window(
        root.root_depth,
        window,
        root.root,
        0,
        0,
        240,
        160,
        0,
        WindowClass::INPUT_OUTPUT,
        root.root_visual,
        &CreateWindowAux::new().background_pixel(0x253040),
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"roost-hints-probe\0RoostHintsProbe\0",
    )
    .unwrap();
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"Legacy Icon Probe",
    )
    .unwrap();
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(root.root_depth, pixmap, window, 32, 32)
        .unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, pixmap, &CreateGCAux::new().foreground(0x20d090))
        .unwrap();
    conn.poly_fill_rectangle(
        pixmap,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 32,
            height: 32,
        }],
    )
    .unwrap();
    conn.change_property32(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_HINTS,
        AtomEnum::WM_HINTS,
        &[1 << 2, 0, 0, pixmap, 0, 0, 0, 0, 0],
    )
    .unwrap();
    conn.map_window(window).unwrap();
    conn.flush().unwrap();
    loop {
        conn.wait_for_event().unwrap();
    }
}
