//! Real wl_surface.frame wire timing, including a one-second client pause.
use std::{
    io::Write,
    os::fd::AsFd,
    time::{Duration, Instant},
};
use wayland_client::{
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
#[derive(Default)]
struct Probe {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm: Option<XdgWmBase>,
    configured: bool,
    timestamps: Vec<(u32, u64, u64)>,
    started: Option<Instant>,
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
            s.configured = true;
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
impl Dispatch<WlCallback, ()> for Probe {
    fn event(
        s: &mut Self,
        _: &WlCallback,
        e: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { callback_data } = e {
            let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
            let monotonic_ms = now.tv_sec as u64 * 1000 + now.tv_nsec as u64 / 1_000_000;
            let elapsed_ms = s.started.unwrap().elapsed().as_millis() as u64;
            s.timestamps.push((callback_data, elapsed_ms, monotonic_ms));
        }
    }
}
fn main() {
    let conn = Connection::connect_to_env().unwrap();
    let mut queue = conn.new_event_queue();
    let q = queue.handle();
    let mut s = Probe::default();
    conn.display().get_registry(&q, ());
    queue.roundtrip(&mut s).unwrap();
    let surface = s.compositor.as_ref().unwrap().create_surface(&q, ());
    let xdg = s.wm.as_ref().unwrap().get_xdg_surface(&surface, &q, ());
    let top = xdg.get_toplevel(&q, ());
    top.set_app_id("org.roost.FrameTimestampProof".into());
    top.set_title("Frame Timestamp Wire Probe".into());
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(std::env::args().nth(1).expect("SHM scratch file argument"))
        .unwrap();
    for _ in 0..240 * 160 {
        file.write_all(&0xff3584e4u32.to_ne_bytes()).unwrap();
    }
    let pool = s
        .shm
        .as_ref()
        .unwrap()
        .create_pool(file.as_fd(), 240 * 160 * 4, &q, ());
    s.buffer = Some(pool.create_buffer(0, 240, 160, 240 * 4, wl_shm::Format::Argb8888, &q, ()));
    pool.destroy();
    s.surface = Some(surface.clone());
    surface.commit();
    let configured_deadline = Instant::now() + Duration::from_secs(5);
    while !s.configured && Instant::now() < configured_deadline {
        queue.roundtrip(&mut s).unwrap();
    }
    assert!(s.configured, "xdg configure timed out");
    s.started = Some(Instant::now());
    for expected in 1..=15 {
        if expected == 6 {
            std::thread::sleep(Duration::from_millis(1000));
        }
        surface.frame(&q, ());
        surface.damage(0, 0, 240, 160);
        surface.commit();
        let deadline = Instant::now() + Duration::from_secs(5);
        while s.timestamps.len() < expected && Instant::now() < deadline {
            queue.roundtrip(&mut s).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            s.timestamps.len(),
            expected,
            "wire frame callback timed out"
        );
    }
    println!("index,callback_ms,client_elapsed_ms,clock_monotonic_ms");
    for (i, (wire, elapsed, monotonic)) in s.timestamps.iter().enumerate() {
        println!("{i},{wire},{elapsed},{monotonic}");
    }
    let wire_delta = s.timestamps[5].0.wrapping_sub(s.timestamps[4].0);
    let real_delta = s.timestamps[5].1 - s.timestamps[4].1;
    println!("pause_wire_delta_ms={wire_delta};pause_real_delta_ms={real_delta}");
    let total_wire = s
        .timestamps
        .last()
        .unwrap()
        .0
        .wrapping_sub(s.timestamps[0].0);
    let total_real = s.timestamps.last().unwrap().1 - s.timestamps[0].1;
    assert!(
        u64::from(total_wire).abs_diff(total_real) <= 150,
        "callback clock drift: wire={total_wire}ms elapsed={total_real}ms"
    );
    assert!(
        wire_delta >= 850 && u64::from(wire_delta).abs_diff(real_delta) <= 150,
        "callback clock did not follow pause: wire={wire_delta}ms elapsed={real_delta}ms"
    );
    println!(
        "P-FRAME-CLOCK PASS: actual callback deltas track elapsed time and a one-second pause"
    );
}
