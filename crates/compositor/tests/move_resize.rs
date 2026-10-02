//! Interactive move and resize (#58), as GTK header bars drive them.
//!
//! A CSD client starts a move with xdg_toplevel.move and a resize with
//! xdg_toplevel.resize; the compositor then owns the pointer until the
//! button is released. Super+drag moves any window, and dropping a moved
//! window on the top edge maximizes it (GNOME snapping). A real client
//! drives every case. Conventions follow `windows.rs`.

use std::os::unix::net::UnixStream;

use roost_compositor::windows::{
    ManagerInput, WindowLayout, WindowManager, MIN_WINDOW_SIZE, SUPER_LEFT_KEYCODE,
};
use roost_compositor::TestCompositor;
use smithay::utils::{Logical, Point};
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
    xdg_toplevel::{Event as ToplevelEvent, ResizeEdge, XdgToplevel},
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
    /// Every toplevel configure size, in order.
    sizes: Vec<(i32, i32)>,
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

impl Dispatch<XdgToplevel, ()> for Client {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: <XdgToplevel as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ToplevelEvent::Configure { width, height, .. } = event {
            state.sizes.push((width, height));
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
empty_dispatch!(XdgPositioner);

struct Fixture {
    comp: TestCompositor,
    manager: WindowManager,
    conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
    _toplevel_surface: WlSurface,
    _toplevel_xdg: XdgSurface,
    toplevel: XdgToplevel,
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
    // A real output so snapping and work areas have a size.
    comp.state.set_output_size(1280, 800);
    let mut f = Fixture {
        comp,
        manager,
        conn,
        queue,
        client,
        _toplevel_surface: surface,
        _toplevel_xdg: xdg,
        toplevel,
        window_origin: (0, 0).into(),
    };
    sync(&mut f);
    sync(&mut f);
    let (_, geometry) = f.manager.visible_windows()[0].clone();
    f.window_origin = geometry.loc;
    f
}

fn id(f: &Fixture) -> u64 {
    f.manager.model().windows().next().unwrap().id
}

fn geometry(f: &Fixture) -> smithay::utils::Rectangle<i32, Logical> {
    f.manager.geometry(id(f)).unwrap()
}

/// Press inside the window so the client has a serial to start from.
fn press_inside(f: &mut Fixture) -> Point<f64, Logical> {
    let at = (geometry(f).loc + Point::from((40, 10))).to_f64();
    f.manager.pointer_motion(&mut f.comp.state, at, 1);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 2);
    sync(f);
    at
}

#[test]
fn header_drag_moves_the_window() {
    let mut f = fixture();
    let start = geometry(&f);
    let at = press_inside(&mut f);
    f.toplevel._move(f.client.seat.as_ref().unwrap(), 1);
    sync(&mut f);
    assert!(f.manager.grab_active(), "move request starts a grab");
    f.manager
        .pointer_motion(&mut f.comp.state, at + Point::from((100.0, 50.0)), 3);
    assert_eq!(geometry(&f).loc, start.loc + Point::from((100, 50)));
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 4);
    assert!(!f.manager.grab_active(), "release ends the grab");
    sync(&mut f);
    // The client's press opened smithay's implicit click grab; only the
    // matching release closes it (#97).
    assert_eq!(
        f.client.releases, 1,
        "the grab's release reaches the client"
    );
}

#[test]
fn bottom_right_resize_grows_and_configures() {
    let mut f = fixture();
    let start = geometry(&f);
    let at = press_inside(&mut f);
    f.toplevel
        .resize(f.client.seat.as_ref().unwrap(), 1, ResizeEdge::BottomRight);
    sync(&mut f);
    f.manager
        .pointer_motion(&mut f.comp.state, at + Point::from((60.0, 40.0)), 3);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 4);
    sync(&mut f);
    let now = geometry(&f);
    assert_eq!(now.loc, start.loc, "the top-left corner stays");
    assert_eq!(now.size.w, start.size.w + 60);
    assert_eq!(now.size.h, start.size.h + 40);
    assert_eq!(
        f.client.sizes.last().copied(),
        Some((now.size.w, now.size.h)),
        "client was told the new size"
    );
}

#[test]
fn top_left_resize_moves_the_origin_and_respects_the_minimum() {
    let mut f = fixture();
    let start = geometry(&f);
    let at = press_inside(&mut f);
    f.toplevel
        .resize(f.client.seat.as_ref().unwrap(), 1, ResizeEdge::TopLeft);
    sync(&mut f);
    // Drag far past the bottom-right: clamps at the minimum size.
    f.manager
        .pointer_motion(&mut f.comp.state, at + Point::from((5000.0, 5000.0)), 3);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 4);
    let now = geometry(&f);
    assert_eq!((now.size.w, now.size.h), MIN_WINDOW_SIZE);
    assert_eq!(
        now.loc + Point::from((now.size.w, now.size.h)),
        start.loc + Point::from((start.size.w, start.size.h)),
        "the bottom-right corner stays"
    );
}

#[test]
fn super_drag_moves_without_a_client_request() {
    let mut f = fixture();
    let start = geometry(&f);
    let at = (start.loc + Point::from((200, 200))).to_f64();
    f.manager.pointer_motion(&mut f.comp.state, at, 1);
    f.manager.on_input(
        &mut f.comp.state,
        ManagerInput::Key {
            keycode: SUPER_LEFT_KEYCODE,
            pressed: true,
            time: 2,
        },
    );
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 3);
    sync(&mut f);
    assert_eq!(f.client.presses, 0, "Super+press is the compositor's");
    f.manager
        .pointer_motion(&mut f.comp.state, at + Point::from((-30.0, 70.0)), 4);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 5);
    assert_eq!(geometry(&f).loc, start.loc + Point::from((-30, 70)));
    sync(&mut f);
    assert_eq!(f.client.releases, 0, "a swallowed press has no release");
}

#[test]
fn dropping_on_the_top_edge_maximizes() {
    let mut f = fixture();
    let at = press_inside(&mut f);
    f.toplevel._move(f.client.seat.as_ref().unwrap(), 1);
    sync(&mut f);
    f.manager
        .pointer_motion(&mut f.comp.state, (at.x, 0.0).into(), 3);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 4);
    assert_eq!(
        f.manager.window_layout(id(&f)),
        Some(WindowLayout::Maximized)
    );
}

#[test]
fn new_windows_never_open_under_the_top_bar() {
    let f = fixture();
    let g = geometry(&f);
    assert!(
        g.loc.y >= roost_compositor::windows::WORK_AREA_TOP,
        "first window at {g:?} must start inside the work area"
    );
    // First window on an empty workspace: centered horizontally.
    assert_eq!(g.loc.x + g.size.w / 2, 640);
}

#[test]
fn dragging_to_an_edge_shows_gnomes_tile_preview() {
    let mut f = fixture();
    let window = id(&f);
    let at = press_inside(&mut f);
    f.toplevel._move(f.client.seat.as_ref().unwrap(), 1);
    sync(&mut f);
    assert_eq!(f.manager.tile_preview(&f.comp.state), None, "no edge yet");
    // The left edge: the left half of the work area, below the bar.
    f.manager
        .pointer_motion(&mut f.comp.state, Point::from((1.0, at.y + 100.0)), 3);
    let (dragged, rect) = f.manager.tile_preview(&f.comp.state).expect("left preview");
    assert_eq!(dragged, window);
    let top = roost_compositor::windows::WORK_AREA_TOP;
    assert_eq!(
        (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
        (0, top, 640, 800 - top)
    );
    // The top edge: the whole work area.
    f.manager
        .pointer_motion(&mut f.comp.state, Point::from((640.0, 1.0)), 4);
    let (_, rect) = f.manager.tile_preview(&f.comp.state).expect("top preview");
    assert_eq!((rect.size.w, rect.size.h), (1280, 800 - top));
    // Back to the middle: the preview goes, and release tiles nothing.
    f.manager
        .pointer_motion(&mut f.comp.state, Point::from((640.0, 400.0)), 5);
    assert_eq!(f.manager.tile_preview(&f.comp.state), None);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 6);
    assert_eq!(
        f.manager.tile_preview(&f.comp.state),
        None,
        "the grab ended"
    );
}
