//! `wl_data_device_manager` advertisement and dispatch: toolkit
//! clients (GTK, Chromium) refuse a display without this global, so a
//! socketpair client binds the manager, creates a data device and a
//! data source, and offers a mime type. A sync barrier afterwards
//! proves the connection survived (a protocol error would kill the
//! client and exhaust the pump budget).

use std::os::unix::net::UnixStream;

use tuna_compositor::TestCompositor;
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

use tuna_compositor::windows::{ManagerInput, WindowManager};
use wayland_client::protocol::{
    wl_compositor::WlCompositor,
    wl_data_offer::WlDataOffer,
    wl_display::WlDisplay,
    wl_keyboard::{Event as KeyEvent, WlKeyboard},
    wl_surface::WlSurface,
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

/// Bytes the copy side serves; small enough to fit any pipe buffer so
/// the source can write without a concurrent reader.
const PASTE_BYTES: &[u8] = b"tuna-clipboard-round-trip";
/// Same for the primary selection side (distinct so a crossed wire
/// between the two selections fails loudly).
const PRIMARY_BYTES: &[u8] = b"tuna-primary-round-trip";
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
    primary_manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    primary_device: Option<ZwpPrimarySelectionDeviceV1>,
    primary_source: Option<ZwpPrimarySelectionSourceV1>,
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
    /// Primary offers announced to this client, in arrival order.
    primary_offers: Vec<ZwpPrimarySelectionOfferV1>,
    /// Mime types offered across all primary offers.
    primary_offered_mimes: Vec<String>,
    /// Current primary offer, if the server sent one.
    primary_selection: Option<ZwpPrimarySelectionOfferV1>,
    /// Mime types the peer requested from our primary source.
    primary_sent_mimes: Vec<String>,
}

impl ClipClient {
    fn ready(&self) -> bool {
        self.compositor.is_some()
            && self.seat.is_some()
            && self.keyboard.is_some()
            && self.xdg_base.is_some()
            && self.manager.is_some()
            && self.primary_manager.is_some()
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
                "zwp_primary_selection_device_manager_v1" => {
                    state.primary_manager =
                        Some(registry.bind::<ZwpPrimarySelectionDeviceManagerV1, _, _>(
                            name,
                            version,
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

impl Dispatch<ZwpPrimarySelectionDeviceManagerV1, ()> for ClipClient {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionDeviceManagerV1,
        _: <ZwpPrimarySelectionDeviceManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: <ZwpPrimarySelectionDeviceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::Event as PrimaryDeviceEvent;
        match event {
            PrimaryDeviceEvent::DataOffer { offer } => state.primary_offers.push(offer),
            PrimaryDeviceEvent::Selection { id } => state.primary_selection = id,
            _ => {}
        }
    }

    wayland_client::event_created_child!(ClipClient, ZwpPrimarySelectionDeviceV1, [
        wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (ZwpPrimarySelectionOfferV1, ()),
    ]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionOfferV1,
        event: <ZwpPrimarySelectionOfferV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_offer_v1::Event as PrimaryOfferEvent;
        if let PrimaryOfferEvent::Offer { mime_type } = event {
            state.primary_offered_mimes.push(mime_type);
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, ()> for ClipClient {
    fn event(
        state: &mut Self,
        _: &ZwpPrimarySelectionSourceV1,
        event: <ZwpPrimarySelectionSourceV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use std::io::Write;
        use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::Event as PrimarySourceEvent;
        if let PrimarySourceEvent::Send { mime_type, fd } = event {
            state.primary_sent_mimes.push(mime_type);
            let mut file = std::fs::File::from(fd);
            file.write_all(PRIMARY_BYTES).unwrap();
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

/// Map one titled toplevel with both selection devices attached,
/// then drain behind a sync barrier.
fn map_selection_client(
    comp: &mut TestCompositor,
    conn: &Connection,
    queue: &mut EventQueue<ClipClient>,
    client: &mut ClipClient,
    title: &str,
) {
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
    let seat = client.seat.as_ref().unwrap();
    client.device = Some(
        client
            .manager
            .as_ref()
            .unwrap()
            .get_data_device(seat, &qh, ()),
    );
    client.primary_device = Some(client.primary_manager.as_ref().unwrap().get_device(
        seat,
        &qh,
        (),
    ));
    client.synced = false;
    conn.display().sync(&qh, ());
    pump_one(comp, queue, client, |c| c.synced);
}

/// Drain the configure round-trip behind a barrier per client;
/// mapping is only real once both sides acked.
#[allow(clippy::too_many_arguments)]
fn sync_mapped(
    comp: &mut TestCompositor,
    conn_a: &Connection,
    queue_a: &mut EventQueue<ClipClient>,
    client_a: &mut ClipClient,
    conn_b: &Connection,
    queue_b: &mut EventQueue<ClipClient>,
    client_b: &mut ClipClient,
) {
    for (conn, queue, client) in [(conn_a, queue_a, client_a), (conn_b, queue_b, client_b)] {
        let qh = queue.handle();
        client.synced = false;
        conn.display().sync(&qh, ());
        pump_one(comp, queue, client, |c| c.synced);
        assert!(client.configured, "toplevel never configured");
        assert!(client.surface.is_some(), "surface dropped");
    }
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

    map_selection_client(&mut comp, &conn_a, &mut queue_a, &mut client_a, "copy");
    map_selection_client(&mut comp, &conn_b, &mut queue_b, &mut client_b, "paste");
    manager.reconcile(&mut comp.state);
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
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

/// Middle-click select in the focused client pastes in the
/// next-focused one, on the primary selection beside the clipboard.
#[test]
fn primary_selection_round_trips_between_focused_clients() {
    use std::io::Read;
    use std::os::fd::AsFd;

    let mut comp = TestCompositor::new();
    let mut manager: WindowManager = comp.window_manager();

    let (conn_a, mut queue_a, mut client_a) = clip_connect(&mut comp);
    let (conn_b, mut queue_b, mut client_b) = clip_connect(&mut comp);

    map_selection_client(&mut comp, &conn_a, &mut queue_a, &mut client_a, "select");
    map_selection_client(
        &mut comp,
        &conn_b,
        &mut queue_b,
        &mut client_b,
        "middle-paste",
    );
    manager.reconcile(&mut comp.state);
    sync_mapped(
        &mut comp,
        &conn_a,
        &mut queue_a,
        &mut client_a,
        &conn_b,
        &mut queue_b,
        &mut client_b,
    );
    let id_a = manager
        .model()
        .windows()
        .find(|w| w.title == "select")
        .unwrap()
        .id;
    let id_b = manager
        .model()
        .windows()
        .find(|w| w.title == "middle-paste")
        .unwrap()
        .id;

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

    // Select: offer text and take the primary selection while focused.
    let qh_a = queue_a.handle();
    let source = client_a
        .primary_manager
        .as_ref()
        .unwrap()
        .create_source(&qh_a, ());
    source.offer("text/plain".to_owned());
    client_a
        .primary_device
        .as_ref()
        .unwrap()
        .set_selection(Some(&source), serial);
    client_a.primary_source = Some(source);

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
        client_b.primary_selection.is_none(),
        "unfocused client must see no primary selection"
    );

    assert!(manager.focus(&mut comp.state, Some(id_b)));
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |_, b| b.primary_selection.is_some(),
    );
    assert_eq!(
        client_b.primary_offers.len(),
        1,
        "one primary offer per selection, got {}",
        client_b.primary_offers.len()
    );
    assert!(
        client_b
            .primary_offered_mimes
            .contains(&"text/plain".to_owned()),
        "offered mimes: {:?}",
        client_b.primary_offered_mimes
    );

    let (read_end, write_end) = UnixStream::pair().unwrap();
    client_b
        .primary_selection
        .as_ref()
        .unwrap()
        .receive("text/plain".to_owned(), write_end.as_fd());
    pump_both(
        &mut comp,
        &mut queue_a,
        &mut client_a,
        &mut queue_b,
        &mut client_b,
        |a, _| !a.primary_sent_mimes.is_empty(),
    );
    assert_eq!(client_a.primary_sent_mimes, vec!["text/plain".to_owned()]);
    drop(write_end);
    assert!(
        client_a.primary_source.is_some(),
        "primary source must stay alive until the peer receives"
    );
    let mut pasted = Vec::new();
    std::io::BufReader::new(read_end)
        .read_to_end(&mut pasted)
        .unwrap();
    assert_eq!(pasted, PRIMARY_BYTES);
}
