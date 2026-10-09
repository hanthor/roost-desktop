//! Pointer event framing and scrolling, as GTK4 clients need them.
//!
//! wl_seat v5+ clients act on pointer events only at a frame boundary:
//! without wl_pointer.frame, GTK4 silently drops clicks (found while
//! bringing up the GTK shell, ADR 0006). And outside scroll mode the
//! wheel must scroll the client under the pointer, as in GNOME. A real
//! client asserts both. Conventions follow `windows.rs`.

use std::os::unix::net::UnixStream;

use smithay::utils::{Logical, Point};
use tuna_compositor::windows::{ManagerInput, WindowManager, WHEEL_STEP_PX};
use tuna_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_callback::WlCallback,
        wl_compositor::WlCompositor,
        wl_display::WlDisplay,
        wl_keyboard::{Event as KeyEvent, WlKeyboard},
        wl_pointer::{Event as PointerEvent, WlPointer},
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{Event as PopupEvent, XdgPopup},
    xdg_positioner::XdgPositioner,
    xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::XdgWmBase,
};

const PUMP_ROUNDS: usize = 200;
const BTN_LEFT: u32 = 0x110;

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    xdg_base: Option<XdgWmBase>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    synced: bool,
    popup_configure: Option<(i32, i32, i32, i32)>,
    popup_done: bool,
    /// Surface ids the pointer entered, in order.
    pointer_entered: Vec<u32>,
    /// Pointer button presses delivered (any surface).
    presses: u32,
    releases: u32,
    /// Surface ids keyboard focus entered, in order.
    keyboard_entered: Vec<u32>,
    /// wl_pointer.frame events received.
    frames: u32,
    /// (axis, value) for every wl_pointer.axis, vertical = 0.
    axes: Vec<(u32, f64)>,
    /// v120 values for every wl_pointer.axis_value120.
    v120: Vec<i32>,
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some()
            && self.xdg_base.is_some()
            && self.keyboard.is_some()
            && self.pointer.is_some()
    }
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
                    state.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                }
                "wl_seat" => {
                    let seat: WlSeat = registry.bind(name, version, qh, ());
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                    state.pointer = Some(seat.get_pointer(qh, ()));
                    state.seat = Some(seat);
                }
                "xdg_wm_base" => {
                    state.xdg_base = Some(registry.bind(name, version.min(7), qh, ()));
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
        event: <WlCallback as Proxy>::Event,
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
        event: <XdgWmBase as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

/// Each xdg_surface carries its wl_surface so the configure ack can
/// commit the right one.
impl Dispatch<XdgSurface, WlSurface> for Client {
    fn event(
        _: &mut Self,
        xdg_surface: &XdgSurface,
        event: <XdgSurface as Proxy>::Event,
        surface: &WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure { serial } =
            event
        {
            xdg_surface.ack_configure(serial);
            surface.commit();
        }
    }
}

impl Dispatch<XdgPopup, ()> for Client {
    fn event(
        state: &mut Self,
        _: &XdgPopup,
        event: <XdgPopup as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            PopupEvent::Configure {
                x,
                y,
                width,
                height,
            } => state.popup_configure = Some((x, y, width, height)),
            PopupEvent::PopupDone => state.popup_done = true,
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: <WlPointer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            PointerEvent::Enter { surface, .. } => {
                state.pointer_entered.push(surface.id().protocol_id())
            }
            PointerEvent::Frame => state.frames += 1,
            PointerEvent::Axis { axis, value, .. } => {
                let axis = match axis {
                    wayland_client::WEnum::Value(a) => a as u32,
                    wayland_client::WEnum::Unknown(a) => a,
                };
                state.axes.push((axis, value));
            }
            PointerEvent::AxisValue120 { value120, .. } => state.v120.push(value120),
            PointerEvent::Button {
                state: wayland_client::WEnum::Value(s),
                ..
            } => {
                if s == wayland_client::protocol::wl_pointer::ButtonState::Pressed {
                    state.presses += 1;
                } else {
                    state.releases += 1;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let KeyEvent::Enter { surface, .. } = event {
            state.keyboard_entered.push(surface.id().protocol_id());
        }
    }
}

macro_rules! empty_dispatch {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for Client {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as Proxy>::Event,
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
empty_dispatch!(XdgToplevel);
empty_dispatch!(XdgPositioner);

struct Fixture {
    comp: TestCompositor,
    manager: WindowManager,
    conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
    _toplevel_surface: WlSurface,
    _toplevel_xdg: XdgSurface,
    window_origin: Point<i32, Logical>,
}

fn pump(f: &mut Fixture, done: impl Fn(&Client) -> bool) {
    for _ in 0..PUMP_ROUNDS {
        f.queue.flush().unwrap();
        f.comp.pump();
        f.manager.reconcile(&mut f.comp.state);
        f.comp.pump();
        if let Some(guard) = f.queue.prepare_read() {
            guard.read().unwrap();
        }
        f.queue.dispatch_pending(&mut f.client).unwrap();
        if done(&f.client) {
            return;
        }
    }
    panic!("pump budget exhausted");
}

fn sync(f: &mut Fixture) {
    let qh = f.queue.handle();
    f.client.synced = false;
    f.conn.display().sync(&qh, ());
    pump(f, |c| c.synced);
}

fn fixture() -> Fixture {
    let mut comp = TestCompositor::new();
    let manager = comp.window_manager();
    let (server, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    for _ in 0..PUMP_ROUNDS {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            guard.read().unwrap();
        }
        queue.dispatch_pending(&mut client).unwrap();
        if client.ready() {
            break;
        }
    }
    assert!(client.ready());
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg = client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, surface.clone());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("menu-host".into());
    surface.commit();
    let mut f = Fixture {
        comp,
        manager,
        conn,
        queue,
        client,
        _toplevel_surface: surface,
        _toplevel_xdg: xdg,
        window_origin: (0, 0).into(),
    };
    sync(&mut f);
    sync(&mut f);
    let (_, geometry) = f.manager.visible_windows()[0].clone();
    f.window_origin = geometry.loc;
    f
}

fn inside(f: &Fixture) -> Point<f64, Logical> {
    (f.window_origin + Point::from((20, 20))).to_f64()
}

#[test]
fn motion_and_buttons_end_with_a_pointer_frame() {
    let mut f = fixture();
    let at = inside(&f);
    f.manager.pointer_motion(&mut f.comp.state, at, 1);
    sync(&mut f);
    let after_motion = f.client.frames;
    assert!(after_motion >= 1, "motion is framed");
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 2);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 3);
    sync(&mut f);
    assert_eq!(f.client.presses, 1);
    assert_eq!(f.client.releases, 1);
    assert!(
        f.client.frames >= after_motion + 2,
        "press and release each framed"
    );
}

#[test]
fn wheel_scrolls_the_client_under_the_pointer() {
    let mut f = fixture();
    let at = inside(&f);
    f.manager.pointer_motion(&mut f.comp.state, at, 1);
    // One notch down: v120 = 120.
    f.manager.on_input(
        &mut f.comp.state,
        ManagerInput::Axis {
            horizontal: 0.0,
            vertical: 120.0,
            time: 2,
        },
    );
    sync(&mut f);
    assert_eq!(
        f.client.axes,
        vec![(0, WHEEL_STEP_PX)],
        "vertical, one step"
    );
    assert_eq!(f.client.v120, vec![120]);
}

#[test]
fn touchpad_scroll_is_continuous() {
    let mut f = fixture();
    let at = inside(&f);
    f.manager.pointer_motion(&mut f.comp.state, at, 1);
    f.manager.on_input(
        &mut f.comp.state,
        ManagerInput::Axis {
            horizontal: 3.5,
            vertical: 0.0,
            time: 2,
        },
    );
    sync(&mut f);
    assert_eq!(f.client.axes, vec![(1, 3.5)], "horizontal, pixel distance");
    assert!(f.client.v120.is_empty(), "no discrete steps for touchpads");
}
