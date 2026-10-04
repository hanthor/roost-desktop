//! Live graphical proof client: map a SHM window, request inhibition and
//! print the protocol's decisions. The shell proof supplies the consent.
use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::{
    wp::keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
        zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
    },
    xdg::shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
};

#[derive(Default)]
struct Client {
    globals: HashMap<String, (u32, u32)>,
}
impl Client {
    fn bind<I: wayland_client::Proxy + 'static>(
        &self,
        registry: &WlRegistry,
        qh: &QueueHandle<Self>,
        version: u32,
    ) -> I
    where
        Self: Dispatch<I, ()>,
    {
        let (name, offered) = self.globals[I::interface().name];
        registry.bind(name, version.min(offered), qh, ())
    }
}
impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.insert(interface, (name, version));
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
impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for Client {
    fn event(
        _: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => println!("ACTIVE"),
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => println!("INACTIVE"),
            _ => {}
        }
    }
}
wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlSeat);
wayland_client::delegate_noop!(Client: ignore WlShm);
wayland_client::delegate_noop!(Client: ignore WlShmPool);
wayland_client::delegate_noop!(Client: ignore WlBuffer);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore ZwpKeyboardShortcutsInhibitManagerV1);

fn main() {
    let app = std::env::args().nth(1).expect("desktop app id");
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut client = Client::default();
    queue.roundtrip(&mut client).unwrap();
    let compositor: WlCompositor = client.bind(&registry, &qh, 6);
    let wm: XdgWmBase = client.bind(&registry, &qh, 6);
    let shm: WlShm = client.bind(&registry, &qh, 1);
    let seat: WlSeat = client.bind(&registry, &qh, 1);
    let inhibit: ZwpKeyboardShortcutsInhibitManagerV1 = client.bind(&registry, &qh, 1);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    top.set_app_id(app);
    top.set_title("Shortcut consent proof".into());
    surface.commit();
    queue.roundtrip(&mut client).unwrap();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![0x80; 320 * 200 * 4]).unwrap();
    let pool = shm.create_pool(file.as_fd(), 320 * 200 * 4, &qh, ());
    let buffer = pool.create_buffer(0, 320, 200, 320 * 4, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 320, 200);
    surface.commit();
    queue.roundtrip(&mut client).unwrap();
    // The proof controls when to request, after compositor focus settles.
    let ready = std::env::args().nth(2).expect("request trigger path");
    while !std::path::Path::new(&ready).exists() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &qh, ());
    queue.flush().unwrap();
    loop {
        queue.blocking_dispatch(&mut client).unwrap();
    }
}
