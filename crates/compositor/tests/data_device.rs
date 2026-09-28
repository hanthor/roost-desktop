//! `wl_data_device_manager` advertisement and dispatch: toolkit
//! clients (GTK, Chromium) refuse a display without this global, so a
//! socketpair client binds the manager, creates a data device and a
//! data source, and offers a mime type. A sync barrier afterwards
//! proves the connection survived (a protocol error would kill the
//! client and exhaust the pump budget).

use std::os::unix::net::UnixStream;

use rwd_compositor::TestCompositor;
use wayland_client::{
    protocol::{
        wl_callback::WlCallback, wl_data_device::WlDataDevice,
        wl_data_device_manager::WlDataDeviceManager, wl_data_source::WlDataSource,
        wl_registry::WlRegistry, wl_seat::WlSeat,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};

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

const PUMP_ROUNDS: usize = 200;

/// One protocol client: bound globals plus the observations the
/// tests assert on.
#[derive(Default)]
struct Client {
    seat: Option<WlSeat>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    source: Option<WlDataSource>,
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
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, version, qh, ()));
                }
                "wl_data_device_manager" => {
                    state.manager = Some(registry.bind::<WlDataDeviceManager, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ));
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

empty_dispatch!(WlSeat);
empty_dispatch!(WlDataDeviceManager);
empty_dispatch!(WlDataSource);

impl Dispatch<WlDataDevice, ()> for Client {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _event: <WlDataDevice as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Devices only emit offers/selection on real input focus;
        // startup binds quietly.
    }
}

/// Pump the server and drain one client queue until `done` or the
/// round budget runs out.
fn pump(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
    done: impl Fn(&Client) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
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
    panic!("pump budget exhausted");
}

/// The manager is advertised, a device and source dispatch cleanly,
/// and the connection survives a barrier afterwards.
#[test]
fn data_device_manager_binds_and_source_offers() {
    let mut comp = TestCompositor::new();
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server_stream);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    pump(&mut comp, &mut queue, &mut client, |c| {
        c.seat.is_some() && c.manager.is_some()
    });

    let device =
        client
            .manager
            .as_ref()
            .unwrap()
            .get_data_device(client.seat.as_ref().unwrap(), &qh, ());
    let source = client.manager.as_ref().unwrap().create_data_source(&qh, ());
    source.offer("text/plain".to_owned());
    client.device = Some(device);
    client.source = Some(source);

    client.synced = false;
    conn.display().sync(&qh, ());
    pump(&mut comp, &mut queue, &mut client, |c| c.synced);
    assert!(client.device.is_some());
    assert!(client.source.is_some());
}

// --- Copy/paste round-trip between two focused clients ---

use rwd_compositor::windows::{ManagerInput, WindowManager};
use wayland_client::protocol::{
    wl_compositor::WlCompositor,
    wl_data_offer::WlDataOffer,
    wl_display::WlDisplay,
    wl_keyboard::{Event as KeyEvent, WlKeyboard},
    wl_surface::WlSurface,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

/// Bytes the copy side serves; small enough to fit any pipe buffer so
/// the source can write without a concurrent reader.
const PASTE_BYTES: &[u8] = b"rwd-clipboard-round-trip";
/// Evdev `a`: a bare key whose press hands the copy side an honest
/// serial for `set_selection`.
const KEY_A: u32 = 30;

/// One clipboard client: a mapped toplevel plus the data-device ends
/// and every observation the round-trip asserts on.
#[derive(Default)]
struct ClipClient {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    keyboard: Option<WlKeyboard>,
    xdg_base: Option<XdgWmBase>,
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    source: Option<WlDataSource>,
    surface: Option<WlSurface>,
    synced: bool,
    configured: bool,
    /// (keycode, serial) in arrival order.
    keys: Vec<(u32, u32)>,
    /// Offers announced to this client, in arrival order.
    offers: Vec<WlDataOffer>,
    /// Mime types offered across all offers.
    offered_mimes: Vec<String>,
    /// Current selection offer, if the server sent one.
    selection: Option<WlDataOffer>,
    /// Mime types the peer requested from our source.
    sent_mimes: Vec<String>,
}

impl ClipClient {
    fn ready(&self) -> bool {
        self.compositor.is_some()
            && self.seat.is_some()
            && self.keyboard.is_some()
            && self.xdg_base.is_some()
            && self.manager.is_some()
    }
}

macro_rules! clip_empty {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for ClipClient {
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

clip_empty!(WlDisplay);
clip_empty!(WlCompositor);
clip_empty!(WlSurface);
clip_empty!(WlSeat);
clip_empty!(WlDataDeviceManager);
clip_empty!(XdgToplevel);

impl Dispatch<WlRegistry, ()> for ClipClient {
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
                "wl_seat" => {
                    let seat = registry.bind::<WlSeat, _, _>(name, version, qh, ());
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                    state.seat = Some(seat);
                }
                "xdg_wm_base" => {
                    state.xdg_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version.min(7), qh, ()));
                }
                "wl_data_device_manager" => {
                    state.manager = Some(registry.bind::<WlDataDeviceManager, _, _>(
                        name,
                        version.min(3),
                        qh,
                        (),
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlCallback, ()> for ClipClient {
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

impl Dispatch<XdgWmBase, ()> for ClipClient {
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

impl Dispatch<XdgSurface, ()> for ClipClient {
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
            state.configured = true;
        }
    }
}

impl Dispatch<WlKeyboard, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: <WlKeyboard as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let KeyEvent::Key { key, serial, .. } = event {
            state.keys.push((key, serial));
        }
    }
}

impl Dispatch<WlDataDevice, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: <WlDataDevice as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_data_device::Event as DeviceEvent;
        match event {
            DeviceEvent::DataOffer { id } => state.offers.push(id),
            DeviceEvent::Selection { id } => state.selection = id,
            _ => {}
        }
    }

    wayland_client::event_created_child!(ClipClient, WlDataDevice, [
        wayland_client::protocol::wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlDataOffer,
        event: <WlDataOffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_data_offer::Event as OfferEvent;
        if let OfferEvent::Offer { mime_type } = event {
            state.offered_mimes.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &WlDataSource,
        event: <WlDataSource as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use std::io::Write;
        use wayland_client::protocol::wl_data_source::Event as SourceEvent;
        if let SourceEvent::Send { mime_type, fd } = event {
            state.sent_mimes.push(mime_type);
            // Small payload: one write, then drop the fd so the
            // reader sees EOF.
            let mut file = std::fs::File::from(fd);
            file.write_all(PASTE_BYTES).unwrap();
        }
    }
}

/// Pump the server and drain one clipboard client's queue until
/// `done` or the round budget runs out.
fn pump_one(
    comp: &mut TestCompositor,
    queue: &mut EventQueue<ClipClient>,
    client: &mut ClipClient,
    done: impl Fn(&ClipClient) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
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
    panic!("pump budget exhausted");
}

/// Pump the server and drain both client queues until `done` or the
/// round budget runs out.
fn pump_both(
    comp: &mut TestCompositor,
    queue_a: &mut EventQueue<ClipClient>,
    client_a: &mut ClipClient,
    queue_b: &mut EventQueue<ClipClient>,
    client_b: &mut ClipClient,
    done: impl Fn(&ClipClient, &ClipClient) -> bool,
) {
    for _ in 0..PUMP_ROUNDS {
        queue_a.flush().unwrap();
        queue_b.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue_a.prepare_read() {
            guard.read().unwrap();
        }
        if let Some(guard) = queue_b.prepare_read() {
            guard.read().unwrap();
        }
        queue_a.dispatch_pending(client_a).unwrap();
        queue_b.dispatch_pending(client_b).unwrap();
        if done(client_a, client_b) {
            return;
        }
    }
    panic!("pump budget exhausted");
}

/// Connect one clipboard client and bind its globals.
fn clip_connect(comp: &mut TestCompositor) -> (Connection, EventQueue<ClipClient>, ClipClient) {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server_stream);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = ClipClient::default();
    conn.display().get_registry(&qh, ());
    pump_one(comp, &mut queue, &mut client, |c| c.ready());
    (conn, queue, client)
}

/// Copy in the focused client pastes in the next-focused one: the
/// offer, mime, and bytes all cross through the compositor.
#[test]
fn clipboard_copy_paste_round_trips_between_focused_clients() {
    use std::io::Read;

    let mut comp = TestCompositor::new();
    // Manager first so seat capabilities exist before the clients'
    // `get_keyboard` requests arrive (mirrors `Runtime::launch`).
    let mut manager: WindowManager = comp.window_manager();

    let (conn_a, mut queue_a, mut client_a) = clip_connect(&mut comp);
    let (conn_b, mut queue_b, mut client_b) = clip_connect(&mut comp);

    for (conn, queue, client, title) in [
        (&conn_a, &mut queue_a, &mut client_a, "copy"),
        (&conn_b, &mut queue_b, &mut client_b, "paste"),
    ] {
        let qh = queue.handle();
        let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
        let xdg_surface = client
            .xdg_base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_title(title.to_owned());
        surface.commit();
        client.surface = Some(surface);
        client.device = Some(client.manager.as_ref().unwrap().get_data_device(
            client.seat.as_ref().unwrap(),
            &qh,
            (),
        ));
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, queue, client, |c| c.synced);
    }
    manager.reconcile(&mut comp.state);
    // The configure round-trip drains behind a barrier per client;
    // mapping is only real once both sides acked.
    for (conn, queue, client) in [
        (&conn_a, &mut queue_a, &mut client_a),
        (&conn_b, &mut queue_b, &mut client_b),
    ] {
        let qh = queue.handle();
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(&mut comp, queue, client, |c| c.synced);
        assert!(client.configured, "toplevel never configured");
        assert!(client.surface.is_some(), "surface dropped");
    }
    let id_a = manager
        .model()
        .windows()
        .find(|w| w.title == "copy")
        .unwrap()
        .id;
    let id_b = manager
        .model()
        .windows()
        .find(|w| w.title == "paste")
        .unwrap()
        .id;

    // Focus the copy side and press a key so `set_selection` carries
    // an honest serial from a real key event.
    assert!(manager.focus(&mut comp.state, Some(id_a)));
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode: KEY_A,
            pressed: true,
            time: 5000,
        },
    );
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |a, _| !a.keys.is_empty(),
    );
    let serial = client_a.keys[0].1;

    // Copy: offer text and take the selection while focused.
    let qh_a = queue_a.handle();
    let source = client_a
        .manager
        .as_ref()
        .unwrap()
        .create_data_source(&qh_a, ());
    source.offer("text/plain".to_owned());
    client_a
        .device
        .as_ref()
        .unwrap()
        .set_selection(Some(&source), serial);
    client_a.source = Some(source);

    // Paste side gets nothing before it holds focus.
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode: KEY_A,
            pressed: false,
            time: 5001,
        },
    );
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |_, _| true,
    );
    assert!(
        client_b.selection.is_none(),
        "unfocused client must see no selection"
    );

    // Focusing the paste side delivers the offer plus selection.
    assert!(manager.focus(&mut comp.state, Some(id_b)));
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |_, b| b.selection.is_some(),
    );
    assert_eq!(
        client_b.offers.len(),
        1,
        "one offer per selection, got {}",
        client_b.offers.len()
    );
    assert!(
        client_b.offered_mimes.contains(&"text/plain".to_owned()),
        "offered mimes: {:?}",
        client_b.offered_mimes
    );

    // Paste: receive into a socketpair; the copy side's `send` writes
    // the bytes, dropping its fd marks EOF for the reader below.
    use std::os::fd::AsFd;
    let (read_end, write_end) = UnixStream::pair().unwrap();
    client_b
        .selection
        .as_ref()
        .unwrap()
        .receive("text/plain".to_owned(), write_end.as_fd());
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |a, _| !a.sent_mimes.is_empty(),
    );
    assert_eq!(client_a.sent_mimes, vec!["text/plain".to_owned()]);
    // The request fd went over SCM at flush time; dropping our copy
    // leaves the source's write as the last writer, so EOF follows it.
    drop(write_end);
    assert!(
        client_a.source.is_some(),
        "source must stay alive until the peer receives"
    );
    let mut pasted = Vec::new();
    std::io::BufReader::new(read_end)
        .read_to_end(&mut pasted)
        .unwrap();
    assert_eq!(pasted, PASTE_BYTES);
}
