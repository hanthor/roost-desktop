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
