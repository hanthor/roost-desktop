//! xdg popups (#88): configure, placement, hit-testing, grab dismissal.
//!
//! A real protocol client maps a toplevel, opens an xdg_popup anchored
//! inside it, and the test checks what a GTK menu needs: the popup is
//! configured where the positioner asked, the pointer finds it there
//! ahead of the window, and a press outside a grabbed popup sends
//! popup_done and is not delivered to the window beneath. Conventions
//! follow `windows.rs`: bounded pump, no sleeps.

use std::os::unix::net::UnixStream;

use roost_compositor::windows::WindowManager;
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
    xdg_positioner::{Anchor, Gravity, XdgPositioner},
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
    toplevel_surface: WlSurface,
    toplevel_xdg: XdgSurface,
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
        toplevel_surface: surface,
        toplevel_xdg: xdg,
        window_origin: (0, 0).into(),
    };
    sync(&mut f);
    sync(&mut f);
    let (_, geometry) = f.manager.visible_windows()[0].clone();
    f.window_origin = geometry.loc;
    f
}

/// Open a 100x50 popup whose top-left sits at (10, 10) in the parent's
/// window geometry, optionally grabbing.
fn open_popup(f: &mut Fixture, grab: bool) -> (WlSurface, XdgPopup) {
    let qh = f.queue.handle();
    let base = f.client.xdg_base.clone().unwrap();
    let positioner = base.create_positioner(&qh, ());
    positioner.set_size(100, 50);
    positioner.set_anchor_rect(10, 10, 1, 1);
    positioner.set_anchor(Anchor::TopLeft);
    positioner.set_gravity(Gravity::BottomRight);
    let surface = f
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = base.get_xdg_surface(&surface, &qh, surface.clone());
    let popup = xdg.get_popup(Some(&f.toplevel_xdg), &positioner, &qh, ());
    if grab {
        popup.grab(f.client.seat.as_ref().unwrap(), 1);
    }
    surface.commit();
    pump(f, |c| c.popup_configure.is_some());
    sync(f);
    (surface, popup)
}

#[test]
fn popup_is_configured_where_the_positioner_asked() {
    let mut f = fixture();
    let _ = open_popup(&mut f, false);
    assert_eq!(f.client.popup_configure, Some((10, 10, 100, 50)));
}

#[test]
fn pointer_finds_the_popup_ahead_of_its_window() {
    let mut f = fixture();
    let (popup_surface, _popup) = open_popup(&mut f, false);
    let inside = f.window_origin + Point::from((20, 20));
    let placed = f
        .manager
        .popup_at(&f.comp.state, inside.to_f64())
        .expect("popup under the pointer");
    assert_eq!(placed.rect.loc, f.window_origin + Point::from((10, 10)));
    // Pointer motion enters the popup surface, not the toplevel.
    f.manager
        .pointer_motion(&mut f.comp.state, inside.to_f64(), 1);
    sync(&mut f);
    assert_eq!(
        f.client.pointer_entered.last().copied(),
        Some(popup_surface.id().protocol_id())
    );
    // Just outside the popup rect: no popup.
    let outside = f.window_origin + Point::from((200, 200));
    assert!(f
        .manager
        .popup_at(&f.comp.state, outside.to_f64())
        .is_none());
}

/// A second client with its own mapped toplevel; returns where to
/// click so the press lands on that client's window only.
struct Other {
    _conn: Connection,
    queue: EventQueue<Client>,
    client: Client,
}

fn other_client(f: &mut Fixture) -> (Other, Point<f64, Logical>) {
    let (server, stream) = UnixStream::pair().unwrap();
    f.comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    let step = |f: &mut Fixture, q: &mut EventQueue<Client>, c: &mut Client| {
        q.flush().unwrap();
        f.comp.pump();
        f.manager.reconcile(&mut f.comp.state);
        f.comp.pump();
        if let Some(guard) = q.prepare_read() {
            guard.read().unwrap();
        }
        q.dispatch_pending(c).unwrap();
    };
    for _ in 0..PUMP_ROUNDS {
        step(f, &mut queue, &mut client);
        if client.ready() {
            break;
        }
    }
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    let xdg = client
        .xdg_base
        .as_ref()
        .unwrap()
        .get_xdg_surface(&surface, &qh, surface.clone());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("other".into());
    surface.commit();
    for _ in 0..20 {
        step(f, &mut queue, &mut client);
    }
    // Keep the original popup client in sync too.
    sync(f);
    assert_eq!(f.manager.visible_windows().len(), 2);
    let (_, host) = f
        .manager
        .visible_windows()
        .into_iter()
        .find(|(_, g)| g.loc == f.window_origin)
        .unwrap();
    let (_, other) = f
        .manager
        .visible_windows()
        .into_iter()
        .find(|(_, g)| g.loc != f.window_origin)
        .unwrap();
    let probe = Point::<i32, Logical>::from((
        other.loc.x + other.size.w - 5,
        other.loc.y + other.size.h - 5,
    ));
    assert!(!host.contains(probe), "probe must miss the menu host");
    (
        Other {
            _conn: conn,
            queue,
            client,
        },
        probe.to_f64(),
    )
}

#[test]
fn press_on_another_client_dismisses_the_grab_and_is_consumed() {
    let mut f = fixture();
    let (_popup_surface, _popup) = open_popup(&mut f, true);
    assert!(f.comp.state.popup_grab_active());
    let (mut other, probe) = other_client(&mut f);
    f.manager.pointer_motion(&mut f.comp.state, probe, 2);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 3);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 4);
    pump(&mut f, |c| c.popup_done);
    for _ in 0..10 {
        other.queue.flush().unwrap();
        f.comp.pump();
        if let Some(guard) = other.queue.prepare_read() {
            guard.read().unwrap();
        }
        other.queue.dispatch_pending(&mut other.client).unwrap();
    }
    assert!(!f.comp.state.popup_grab_active());
    assert_eq!(other.client.presses, 0, "dismissing press consumed");
    assert_eq!(other.client.releases, 0, "its release is swallowed too");
}

#[test]
fn press_on_the_grab_owners_own_surface_is_delivered() {
    let mut f = fixture();
    let (_popup_surface, _popup) = open_popup(&mut f, true);
    // Inside the menu host window, outside the popup rect.
    let own = (f.window_origin + Point::from((300, 200))).to_f64();
    assert!(f.manager.popup_at(&f.comp.state, own).is_none());
    f.manager.pointer_motion(&mut f.comp.state, own, 2);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, true, 3);
    f.manager
        .pointer_button(&mut f.comp.state, BTN_LEFT, false, 4);
    sync(&mut f);
    assert!(
        !f.client.popup_done,
        "the owner decides, not the compositor"
    );
    assert_eq!(f.client.presses, 1);
    assert_eq!(f.client.releases, 1);
}

#[test]
fn keyboard_focus_returns_to_the_window_after_the_grab() {
    let mut f = fixture();
    let (_popup_surface, popup) = open_popup(&mut f, true);
    popup.destroy();
    sync(&mut f);
    sync(&mut f);
    assert_eq!(
        f.client.keyboard_entered.last().copied(),
        Some(f.toplevel_surface.id().protocol_id())
    );
}

#[test]
fn a_new_grab_keeps_focus_when_the_old_popup_closed_in_the_same_tick() {
    // A panel closing one menu and opening the next sends both in one
    // flush: the old grab's "hand focus back" must not land on the new
    // popup's keyboard focus, or Escape never reaches the new menu.
    let mut f = fixture();
    let (_first_surface, first) = open_popup(&mut f, true);
    first.destroy();
    f.client.popup_configure = None;
    let (second_surface, _second) = open_popup(&mut f, true);
    sync(&mut f);
    assert_eq!(
        f.client.keyboard_entered.last().copied(),
        Some(second_surface.id().protocol_id()),
        "keyboard focus stays on the newly grabbed popup"
    );
}

#[test]
fn popup_slides_back_onto_the_output() {
    use wayland_protocols::xdg::shell::client::xdg_positioner::ConstraintAdjustment;
    let mut f = fixture();
    f.comp.state.set_output_size(1280, 800);
    sync(&mut f);
    let qh = f.queue.handle();
    let base = f.client.xdg_base.clone().unwrap();
    let positioner = base.create_positioner(&qh, ());
    // 400 wide, anchored far right of the parent: would overflow any
    // output edge unless slid back.
    positioner.set_size(400, 50);
    positioner.set_anchor_rect(5000, 10, 1, 1);
    positioner.set_anchor(Anchor::TopLeft);
    positioner.set_gravity(Gravity::BottomRight);
    positioner.set_constraint_adjustment(ConstraintAdjustment::SlideX);
    let surface = f
        .client
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&qh, ());
    let xdg = base.get_xdg_surface(&surface, &qh, surface.clone());
    let _popup = xdg.get_popup(Some(&f.toplevel_xdg), &positioner, &qh, ());
    surface.commit();
    pump(&mut f, |c| c.popup_configure.is_some());
    let (x, _, w, _) = f.client.popup_configure.unwrap();
    let output_w = 1280;
    let right_edge = f.window_origin.x + x + w;
    assert!(
        right_edge <= output_w,
        "popup right edge {right_edge} slid inside the {output_w}px output"
    );
    assert_eq!(w, 400, "slide keeps the size");
}
