//! Original Wayland bell principal for graphical and audio qualification.
//! Map a fixed RGB240 window; consume monotonically numbered window/whole
//! requests from a bounded control file and log completed wire roundtrips.
//! Wire completion is not proof of presented pixels or played audio.
use std::collections::HashMap;
use std::io::Write;
use std::os::fd::AsFd;
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
use wayland_protocols::{
    xdg::shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
    xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1,
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
wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlShm);
wayland_client::delegate_noop!(Client: ignore WlShmPool);
wayland_client::delegate_noop!(Client: ignore WlBuffer);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore XdgSystemBellV1);

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
    let bell: XdgSystemBellV1 = client.bind(&registry, &qh, 1);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let top = xdg.get_toplevel(&qh, ());
    top.set_app_id(app);
    top.set_title("Original system bell principal".into());
    surface.commit();
    queue.roundtrip(&mut client).unwrap();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&vec![0xf0; 320 * 200 * 4]).unwrap();
    let pool = shm.create_pool(file.as_fd(), 320 * 200 * 4, &qh, ());
    let buffer = pool.create_buffer(0, 320, 200, 320 * 4, wl_shm::Format::Xrgb8888, &qh, ());
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 320, 200);
    surface.commit();
    queue.roundtrip(&mut client).unwrap();
    let control = std::env::args().nth(2).expect("bell request control file");
    let started = std::time::Instant::now();
    let mut previous = 0u64;
    println!("READY pid={} rgb=240 size=320x200", std::process::id());
    std::io::stdout().flush().unwrap();
    loop {
        // Keep the original mapped connection dispatching; a completed
        // roundtrip establishes that the real server handled each request.
        queue.roundtrip(&mut client).unwrap();
        if let Ok(file) = std::fs::File::open(&control) {
            use std::io::Read;
            let mut request = String::new();
            file.take(128).read_to_string(&mut request).unwrap();
            let mut fields = request.split_whitespace();
            let parsed = match (fields.next(), fields.next(), fields.next()) {
                (Some(sequence), Some(target @ ("window" | "whole")), None) => sequence
                    .parse::<u64>()
                    .ok()
                    .filter(|sequence| *sequence > previous)
                    .map(|sequence| (sequence, target)),
                _ => None,
            };
            if let Some((sequence, target)) = parsed {
                bell.ring((target == "window").then_some(&surface));
                queue.roundtrip(&mut client).unwrap();
                previous = sequence;
                println!(
                    "RING seq={sequence} target={target} elapsed_us={}",
                    started.elapsed().as_micros()
                );
                std::io::stdout().flush().unwrap();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
