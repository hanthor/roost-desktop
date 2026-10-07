//! Real socket and protocol coverage of service-channel allocation and tags.
use super::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::xdg::{
    shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
    toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1,
};

fn channel(comp: &crate::TestCompositor) -> ServiceChannel {
    ServiceChannel {
        display: comp.display.handle(),
        authority: Default::default(),
        clients: HashMap::new(),
    }
}
fn stream(fd: zbus::zvariant::OwnedFd) -> UnixStream {
    UnixStream::from(std::os::fd::OwnedFd::from(fd))
}
#[test]
fn service_connection_transports_real_sync_and_reclaims_disconnected_slots() {
    let mut comp = crate::TestCompositor::new();
    let mut channel = channel(&comp);
    let mut socket = stream(
        channel
            .open_connection(":1.1".into(), false, None, None)
            .unwrap(),
    );
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let request: Vec<u8> = [1u32, 12 << 16, 2]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect();
    socket.write_all(&request).unwrap();
    comp.pump();
    let mut events = [0u8; 24];
    socket.read_exact(&mut events).unwrap();
    let (chunks, remainder) = events.as_chunks::<4>();
    assert!(remainder.is_empty());
    let words: Vec<u32> = chunks.iter().copied().map(u32::from_ne_bytes).collect();
    assert_eq!(&words[..2], &[2, 12 << 16]); // wl_callback.done
    assert_eq!(&words[3..], &[1, (12 << 16) | 1, 2]); // wl_display.delete_id
    let mut sockets = vec![socket];
    for _ in 1..32 {
        sockets.push(stream(
            channel
                .open_connection(":1.1".into(), false, None, None)
                .unwrap(),
        ));
    }
    assert!(matches!(
        channel.open_connection(":1.2".into(), false, None, None),
        Err(fdo::Error::LimitsExceeded(_))
    ));
    drop(sockets.pop());
    comp.pump();
    let replacement = channel
        .open_connection(":1.2".into(), false, None, None)
        .unwrap();
    assert_eq!(channel.clients.values().map(Vec::len).sum::<usize>(), 32);
    drop(replacement);
}
#[test]
fn typed_service_limit_is_independent_of_ordinary_connections() {
    let comp = crate::TestCompositor::new();
    let mut channel = channel(&comp);
    let _ordinary = channel
        .open_connection(":1.1".into(), false, None, None)
        .unwrap();
    let _typed = channel
        .open_connection(":1.1".into(), true, None, None)
        .unwrap();
    assert!(matches!(
        channel.open_connection(":1.1".into(), true, None, None),
        Err(fdo::Error::LimitsExceeded(_))
    ));
    let _second_ordinary = channel
        .open_connection(":1.1".into(), false, None, None)
        .unwrap();
}
#[test]
fn window_tag_options_follow_mutter_types_and_bound_storage() {
    let mut options = HashMap::from([("unknown".into(), OwnedValue::from(42u32))]);
    assert_eq!(connection_window_tag(&options).unwrap(), None);
    options.insert("window-tag".into(), OwnedValue::from(42u32));
    assert_eq!(connection_window_tag(&options).unwrap(), None);
    for tag in ["", "file-picker", &"a".repeat(1024)] {
        options.insert(
            "window-tag".into(),
            OwnedValue::try_from(Value::from(tag)).unwrap(),
        );
        assert_eq!(
            connection_window_tag(&options).unwrap().as_deref(),
            Some(tag)
        );
    }
    options.insert(
        "window-tag".into(),
        OwnedValue::try_from(Value::from("a".repeat(1025))).unwrap(),
    );
    assert!(matches!(
        connection_window_tag(&options),
        Err(fdo::Error::InvalidArgs(_))
    ));
}

#[derive(Default)]
struct TagPeer {
    globals: HashMap<String, (u32, u32)>,
}
impl Dispatch<WlRegistry, ()> for TagPeer {
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
impl Dispatch<XdgSurface, ()> for TagPeer {
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
impl Dispatch<XdgWmBase, ()> for TagPeer {
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
wayland_client::delegate_noop!(TagPeer: ignore WlCompositor);
wayland_client::delegate_noop!(TagPeer: ignore WlSurface);
wayland_client::delegate_noop!(TagPeer: ignore XdgToplevel);
wayland_client::delegate_noop!(TagPeer: ignore XdgToplevelTagManagerV1);

fn pump(
    comp: &mut crate::TestCompositor,
    queue: &mut wayland_client::EventQueue<TagPeer>,
    peer: &mut TagPeer,
) {
    for _ in 0..10 {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            let _ = guard.read();
        }
        queue.dispatch_pending(peer).unwrap();
    }
}

#[test]
fn connection_tags_reach_real_windows_and_explicit_tags_take_precedence() {
    let mut comp = crate::TestCompositor::new();
    for inherited in [Some("file-picker"), None] {
        let (server, socket) = UnixStream::pair().unwrap();
        let alive = Arc::new(AtomicBool::new(true));
        let client = comp
            .display
            .handle()
            .insert_client(
                server,
                Arc::new(crate::ClientState::service_connection(
                    alive,
                    inherited.map(str::to_owned),
                )),
            )
            .unwrap();
        assert!(!client.get_data::<crate::ClientState>().unwrap().ime_bridge);
        let conn = Connection::from_socket(socket).unwrap();
        let mut queue = conn.new_event_queue::<TagPeer>();
        let registry = conn.display().get_registry(&queue.handle(), ());
        let mut peer = TagPeer::default();
        pump(&mut comp, &mut queue, &mut peer);
        let qh = queue.handle();
        let compositor: WlCompositor = registry.bind(peer.globals["wl_compositor"].0, 6, &qh, ());
        let wm: XdgWmBase = registry.bind(peer.globals["xdg_wm_base"].0, 6, &qh, ());
        let tags: XdgToplevelTagManagerV1 =
            registry.bind(peer.globals["xdg_toplevel_tag_manager_v1"].0, 1, &qh, ());
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        surface.commit();
        pump(&mut comp, &mut queue, &mut peer);
        let server_surface = client.object_from_protocol_id::<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>(&comp.display.handle(), surface.id().protocol_id()).unwrap();
        assert_eq!(
            crate::protocols::toplevel_tag(&server_surface)
                .tag
                .as_deref(),
            inherited
        );
        tags.set_toplevel_description(&toplevel, "Chooser".into());
        tags.set_toplevel_tag(&toplevel, "explicit".into());
        pump(&mut comp, &mut queue, &mut peer);
        let result = crate::protocols::toplevel_tag(&server_surface);
        assert_eq!(result.tag.as_deref(), Some("explicit"));
        assert_eq!(result.description.as_deref(), Some("Chooser"));
    }
}

#[test]
fn service_window_keeps_original_pinned_process_instead_of_socketpair_creator() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let pid = child.id();
    let descriptor = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(pid as i32).unwrap(),
        rustix::process::PidfdFlags::empty(),
    )
    .unwrap();
    let credentials = Arc::new(
        fdo::ConnectionCredentials::default()
            .set_process_id(pid)
            .set_process_fd(descriptor.into()),
    );
    let mut comp = crate::TestCompositor::new();
    let (server, socket) = UnixStream::pair().unwrap();
    let mut data = crate::ClientState::service_connection(Arc::new(AtomicBool::new(true)), None);
    data.original_credentials = Some(credentials);
    // Cached fixture metadata tests live-pin expiration, not real Flatpak discovery.
    data.sandboxed_app_id
        .set(Some("org.example.CachedFixture".into()))
        .unwrap();
    assert!(!data.ime_bridge);
    assert!(!data.x11_interop);
    let client = comp
        .display
        .handle()
        .insert_client(server, Arc::new(data))
        .unwrap();
    let conn = Connection::from_socket(socket).unwrap();
    let mut queue = conn.new_event_queue::<TagPeer>();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let mut peer = TagPeer::default();
    pump(&mut comp, &mut queue, &mut peer);
    let compositor: WlCompositor =
        registry.bind(peer.globals["wl_compositor"].0, 6, &queue.handle(), ());
    let surface = compositor.create_surface(&queue.handle(), ());
    pump(&mut comp, &mut queue, &mut peer);
    let server_surface = client.object_from_protocol_id::<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>(&comp.display.handle(), surface.id().protocol_id()).unwrap();
    let socket_creator = client.get_credentials(&comp.display.handle()).unwrap().pid;
    let original = comp.state.authenticated_service_client_pid(&server_surface);
    let sandbox = comp.state.authenticated_sandboxed_app_id(&server_surface);
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(socket_creator, std::process::id() as i32);
    assert_ne!(pid, std::process::id());
    assert_eq!(original, Some(pid));
    assert_eq!(sandbox.as_deref(), Some("org.example.CachedFixture"));
    assert_eq!(
        comp.state.authenticated_sandboxed_app_id(&server_surface),
        None
    );
    assert_eq!(
        comp.state.authenticated_service_client_pid(&server_surface),
        None
    );
}

#[cfg(feature = "xwayland")]
#[test]
fn x11_interop_registry_requires_typed_admission_not_service_liveness() {
    let mut comp = crate::TestCompositor::new();
    let mut service = channel(&comp);
    for typed in [false, true] {
        let fd = service
            .open_connection(format!(":1.{}", typed as u8), typed, None, None)
            .unwrap();
        let conn = Connection::from_socket(stream(fd)).unwrap();
        let mut queue = conn.new_event_queue::<TagPeer>();
        conn.display().get_registry(&queue.handle(), ());
        let mut peer = TagPeer::default();
        pump(&mut comp, &mut queue, &mut peer);
        assert_eq!(peer.globals.contains_key("mutter_x11_interop"), typed);
        // Both connections are service-tracked: only the admitted typed one
        // gets the interop capability, without IME or capture roles.
    }
    let (server, socket) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(socket).unwrap();
    let mut queue = conn.new_event_queue::<TagPeer>();
    conn.display().get_registry(&queue.handle(), ());
    let mut peer = TagPeer::default();
    pump(&mut comp, &mut queue, &mut peer);
    assert!(!peer.globals.contains_key("mutter_x11_interop"));
}

#[cfg(feature = "xwayland")]
#[test]
fn ordinary_service_client_cannot_blind_bind_the_trusted_x11_global() {
    let mut comp = crate::TestCompositor::new();
    let mut service = channel(&comp);
    let typed = service
        .open_connection(":1.trusted".into(), true, None, None)
        .unwrap();
    let conn = Connection::from_socket(stream(typed)).unwrap();
    let mut queue = conn.new_event_queue::<TagPeer>();
    conn.display().get_registry(&queue.handle(), ());
    let mut peer = TagPeer::default();
    pump(&mut comp, &mut queue, &mut peer);
    let global = peer.globals["mutter_x11_interop"].0;
    let mut socket = stream(
        service
            .open_connection(":1.blind".into(), false, None, None)
            .unwrap(),
    );
    assert!(service.clients[":1.blind"][0].alive.load(Ordering::SeqCst));
    // Real wl_display.get_registry(new_id=2), then wl_registry.bind of
    // the privileged global even though this client was never told its ID.
    let registry: Vec<u8> = [1u32, (12 << 16) | 1, 2]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect();
    socket.write_all(&registry).unwrap();
    comp.pump();
    let name = b"mutter_x11_interop\0";
    let padded = (name.len() + 3) & !3;
    let size = 8 + 4 + 4 + padded + 4 + 4;
    let mut bind: Vec<u8> = [2u32, (size as u32) << 16, global, name.len() as u32]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect();
    bind.extend_from_slice(name);
    bind.resize(16 + padded, 0);
    bind.extend_from_slice(&1u32.to_ne_bytes());
    bind.extend_from_slice(&3u32.to_ne_bytes());
    socket.write_all(&bind).unwrap();
    comp.pump();
    assert!(
        !service.clients[":1.blind"][0].alive.load(Ordering::SeqCst),
        "blind binding must reject the ordinary connection, not merely hide a registry entry"
    );
}
