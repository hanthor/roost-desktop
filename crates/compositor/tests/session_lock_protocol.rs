//! Real ext-session-lock clients; runtime PID/PAM policy is tested separately.
use roost_compositor::TestCompositor;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use wayland_client::protocol::{
    wl_compositor::WlCompositor,
    wl_output::WlOutput,
    wl_registry::{self, WlRegistry},
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

#[derive(Default)]
struct Client {
    globals: HashMap<String, (u32, u32)>,
    events: Vec<(u32, bool)>,
    configured: Vec<(u32, u32)>,
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
impl Dispatch<ExtSessionLockV1, u32> for Client {
    fn event(
        state: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => state.events.push((*tag, true)),
            ext_session_lock_v1::Event::Finished => state.events.push((*tag, false)),
            _ => (),
        }
    }
}
impl Dispatch<ExtSessionLockSurfaceV1, ()> for Client {
    fn event(
        state: &mut Self,
        surface: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            surface.ack_configure(serial);
            state.configured.push((width, height));
        }
    }
}
wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlOutput);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore ExtSessionLockManagerV1);

struct Peer {
    conn: Connection,
    queue: EventQueue<Client>,
    registry: WlRegistry,
    client: Client,
}
impl Peer {
    fn bind<I>(&self, version: u32) -> I
    where
        I: Proxy + 'static,
        Client: Dispatch<I, ()>,
    {
        let (name, offered) = self.client.globals[I::interface().name];
        self.registry
            .bind(name, version.min(offered), &self.queue.handle(), ())
    }
}
fn pump(comp: &mut TestCompositor, peer: &mut Peer) {
    for _ in 0..10 {
        peer.queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = peer.queue.prepare_read() {
            let _ = guard.read();
        }
        peer.queue.dispatch_pending(&mut peer.client).unwrap();
    }
}
fn connect(comp: &mut TestCompositor) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let mut peer = Peer {
        conn,
        queue,
        registry,
        client: Client::default(),
    };
    pump(comp, &mut peer);
    peer
}
fn compositor() -> TestCompositor {
    let mut comp = TestCompositor::new();
    let output = Output::new(
        "fixture".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Roost".into(),
            model: "Test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (1500, 900).into(),
            refresh: 60_000,
        }),
        None,
        Some(Scale::Fractional(1.5)),
        Some((0, 0).into()),
    );
    output.create_global::<roost_compositor::State>(&comp.display.handle());
    comp.state.add_output("fixture", Some(output), 1500, 900);
    comp
}

#[test]
fn pending_lock_is_not_granted_and_denial_reports_finished() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let lock = manager.lock(&peer.queue.handle(), 1);
    pump(&mut comp, &mut peer);
    assert!(
        peer.client.events.is_empty(),
        "client cannot approve its own request"
    );
    let (pending, pid) = comp
        .state
        .take_lock_request()
        .expect("actual protocol request");
    assert_eq!(
        pid,
        Some(std::process::id() as i32),
        "SO_PEERCRED sender PID"
    );
    assert!(comp.state.take_lock_request().is_none());
    drop(pending);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(1, false)]);
    assert!(!comp.state.has_lock_surfaces());
    assert!(!comp.state.take_client_unlock());
    lock.destroy();
}

#[test]
fn approved_lock_configures_scaled_output_and_unlock_only_requests_release() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let lock = manager.lock(&peer.queue.handle(), 2);
    pump(&mut comp, &mut peer);
    comp.state.take_lock_request().unwrap().0.lock();
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events, vec![(2, true)]);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let surface = compositor.create_surface(&peer.queue.handle(), ());
    let role = lock.get_lock_surface(&surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    assert_eq!(
        peer.client.configured,
        vec![(1000, 600)],
        "physical mode divided by scale"
    );
    assert!(comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surface_for("fixture").is_some());
    assert!(comp.state.lock_surface_for("missing").is_none());
    assert_eq!(comp.state.lock_surfaces().len(), 1);
    assert!(!comp.state.take_client_unlock());
    lock.unlock_and_destroy();
    pump(&mut comp, &mut peer);
    assert!(
        comp.state.take_client_unlock(),
        "notification, not PAM authentication"
    );
    assert!(
        !comp.state.take_client_unlock(),
        "request consumed exactly once"
    );
    assert!(!comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surfaces().is_empty());
    role.destroy();
    surface.destroy();
}

#[test]
fn replacing_pending_requests_finishes_old_owners_without_granting_them() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let locks: Vec<_> = (0..64)
        .map(|tag| manager.lock(&peer.queue.handle(), tag))
        .collect();
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events.len(), 63);
    assert!(peer.client.events.iter().all(|(_, granted)| !granted));
    let (last, _) = comp.state.take_lock_request().unwrap();
    assert!(
        comp.state.take_lock_request().is_none(),
        "only one pending owner retained"
    );
    drop(last);
    pump(&mut comp, &mut peer);
    assert_eq!(peer.client.events.len(), 64);
    assert!(!comp.state.has_lock_surfaces());
    for lock in locks {
        lock.destroy();
    }
    peer.conn.flush().unwrap();
}

#[test]
fn disconnected_lock_client_leaves_no_live_surface_or_unlock_request() {
    let mut comp = compositor();
    let mut peer = connect(&mut comp);
    let manager: ExtSessionLockManagerV1 = peer.bind(1);
    let lock = manager.lock(&peer.queue.handle(), 3);
    pump(&mut comp, &mut peer);
    comp.state.take_lock_request().unwrap().0.lock();
    pump(&mut comp, &mut peer);
    let compositor: WlCompositor = peer.bind(6);
    let output: WlOutput = peer.bind(4);
    let surface = compositor.create_surface(&peer.queue.handle(), ());
    let role = lock.get_lock_surface(&surface, &output, &peer.queue.handle(), ());
    pump(&mut comp, &mut peer);
    assert!(comp.state.has_lock_surfaces());
    drop((role, surface, output, compositor, lock, manager, peer));
    for _ in 0..10 {
        comp.pump();
    }
    assert!(!comp.state.has_lock_surfaces());
    assert!(comp.state.lock_surfaces().is_empty());
    assert!(comp.state.lock_surface_for("fixture").is_none());
    assert!(
        !comp.state.take_client_unlock(),
        "client death is never authentication"
    );
}
