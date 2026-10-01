//! Task 3 hotplug window-migration tests.
//!
//! Removing an output migrates its windows onto a live one before the
//! inventory entry drops, so no window is lost on hotplug. Virtual
//! outputs stand in for hotplugged displays (no restart between
//! add/remove); one test goes through a real `wl_output` global to
//! prove clients see the removal live.
//!
//! Conventions follow `windows.rs`: socketpair clients, a bounded pump
//! budget, plain asserts, no sleeps. All code here is original.

use std::os::unix::net::UnixStream;

use roost_compositor::windows::{WindowLayout, WindowManager};
use roost_compositor::{State, TestCompositor};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use wayland_client::{
    protocol::{
        wl_callback::WlCallback, wl_compositor::WlCompositor, wl_output::WlOutput,
        wl_registry::WlRegistry, wl_seat::WlSeat, wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};

const PUMP_ROUNDS: usize = 200;

/// One protocol client: bound globals plus every observation the
/// migration tests assert on (wl_output advertisements and removals
/// for the real-global test).
#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    seat: Option<WlSeat>,
    xdg_base: Option<XdgWmBase>,
    surface: Option<WlSurface>,
    xdg_surface: Option<XdgSurface>,
    toplevel: Option<XdgToplevel>,
    /// Bound `wl_output` globals, in registry order.
    outputs: Vec<WlOutput>,
    /// Registry names advertised as `wl_output`.
    output_names: Vec<u32>,
    /// Registry names withdrawn via `global_remove`.
    removed_names: Vec<u32>,
    synced: bool,
    configured: bool,
}

impl Client {
    fn ready(&self) -> bool {
        self.compositor.is_some() && self.seat.is_some() && self.xdg_base.is_some()
    }
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
        match event {
            wayland_client::protocol::wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    state.compositor =
                        Some(registry.bind::<WlCompositor, _, _>(name, version.min(6), qh, ()));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind::<WlSeat, _, _>(name, version, qh, ()));
                }
                "xdg_wm_base" => {
                    state.xdg_base =
                        Some(registry.bind::<XdgWmBase, _, _>(name, version.min(7), qh, ()));
                }
                "wl_output" => {
                    state.output_names.push(name);
                    state.outputs.push(registry.bind::<WlOutput, _, _>(
                        name,
                        version.min(4),
                        qh,
                        (),
                    ));
                }
                _ => {}
            },
            wayland_client::protocol::wl_registry::Event::GlobalRemove { name } => {
                state.removed_names.push(name);
            }
            _ => {}
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

impl Dispatch<XdgWmBase, ()> for Client {
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

impl Dispatch<XdgSurface, ()> for Client {
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
            if let Some(surface) = &state.surface {
                surface.commit();
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Client {
    fn event(
        _: &mut Self,
        _: &XdgToplevel,
        _: <XdgToplevel as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

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

empty_dispatch!(WlCompositor);
empty_dispatch!(WlSurface);
empty_dispatch!(WlSeat);
empty_dispatch!(WlOutput);

/// Pump the server and drain one client queue until `done` or the round
/// budget runs out. Single-threaded by design: neither side blocks, and
/// every call site guarantees a server reply first.
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

/// Connect one client and bind globals (compositor, seat, xdg shell,
/// plus any advertised `wl_output`s).
fn connect(comp: &mut TestCompositor) -> (Connection, EventQueue<Client>, Client) {
    let (server_stream, client_stream) = UnixStream::pair().unwrap();
    comp.add_client(server_stream);
    let conn = Connection::from_socket(client_stream).unwrap();
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut client = Client::default();
    conn.display().get_registry(&qh, ());
    pump(comp, &mut queue, &mut client, |c| c.ready());
    (conn, queue, client)
}

/// Map one titled toplevel on an already-connected client.
fn map_toplevel(
    comp: &mut TestCompositor,
    conn: &Connection,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
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
    client.xdg_surface = Some(xdg_surface.clone());
    client.toplevel = Some(toplevel);
    client.configured = false;
    client.synced = false;
    conn.display().sync(&qh, ());
    // Only the sync barrier is awaited here: the initial configure is not
    // sent by the server until `WindowManager::reconcile` maps the
    // surface, so waiting for it now would deadlock.
    pump(comp, queue, client, |c| c.synced);
}

/// Drain one client's pending server messages behind a sync barrier.
fn sync_client(
    comp: &mut TestCompositor,
    conn: &Connection,
    queue: &mut EventQueue<Client>,
    client: &mut Client,
) {
    let qh = queue.handle();
    client.synced = false;
    conn.display().sync(&qh, ());
    pump(comp, queue, client, |c| c.synced);
}

/// Two mapped windows ("alpha" from client A, "beta" from client B) on
/// two virtual outputs: "left" 1280x800 at (0,0, primary) and "right"
/// 1920x1080 at (1280,0). Geometry: alpha at (0,0), beta cascaded to
/// (32,32), both 800x600 floating.
struct Fixture {
    comp: TestCompositor,
    manager: WindowManager,
    _conn_a: Connection,
    _queue_a: EventQueue<Client>,
    _client_a: Client,
    _conn_b: Connection,
    _queue_b: EventQueue<Client>,
    _client_b: Client,
    id_a: u64,
    id_b: u64,
}

fn two_windows_dual() -> Fixture {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);
    // The manager first: its seat capabilities must exist before any
    // client request arrives, mirroring `Runtime::launch`.
    let mut manager = comp.window_manager();
    let (conn_a, mut queue_a, mut client_a) = connect(&mut comp);
    map_toplevel(&mut comp, &conn_a, &mut queue_a, &mut client_a, "alpha");
    let (conn_b, mut queue_b, mut client_b) = connect(&mut comp);
    map_toplevel(&mut comp, &conn_b, &mut queue_b, &mut client_b, "beta");
    manager.reconcile(&mut comp.state);
    sync_client(&mut comp, &conn_a, &mut queue_a, &mut client_a);
    sync_client(&mut comp, &conn_b, &mut queue_b, &mut client_b);
    let id_a = manager
        .model()
        .windows()
        .find(|w| w.title == "alpha")
        .unwrap()
        .id;
    let id_b = manager
        .model()
        .windows()
        .find(|w| w.title == "beta")
        .unwrap()
        .id;
    Fixture {
        comp,
        manager,
        _conn_a: conn_a,
        _queue_a: queue_a,
        _client_a: client_a,
        _conn_b: conn_b,
        _queue_b: queue_b,
        _client_b: client_b,
        id_a,
        id_b,
    }
}

/// Right slice bounds: location (1280,0), size 1920x1080.
fn assert_inside_right(geo: smithay::utils::Rectangle<i32, smithay::utils::Logical>) {
    assert!(
        geo.loc.x >= 1280 && geo.loc.x + geo.size.w <= 1280 + 1920,
        "window x-range inside the right slice, got {geo:?}"
    );
    assert!(
        geo.loc.y >= 0 && geo.loc.y + geo.size.h <= 1080,
        "window y-range inside the right slice, got {geo:?}"
    );
}

#[test]
fn removed_output_shifts_floating_windows_into_survivor() {
    let mut f = two_windows_dual();
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Floating)
    );
    // Park beta on the right slice; alpha stays on the removed slice.
    assert!(f.manager.move_window(f.id_b, 1280, 0));
    let beta_before = f.manager.geometry(f.id_b).unwrap();
    assert_eq!((beta_before.loc.x, beta_before.loc.y), (1312, 32));
    let focus_before = f.manager.model().focused();
    let ws_a = f.manager.model().window(f.id_a).unwrap().workspace;
    let ws_b = f.manager.model().window(f.id_b).unwrap().workspace;

    // Migrate first, then drop the entry: the documented removal order.
    let moved = f.manager.migrate_output_windows(&mut f.comp.state, "left");
    assert_eq!(moved, 1, "only alpha sat on the removed slice");
    assert!(f.comp.state.remove_output("left"));

    // No window is lost: both stay in the model with focus and
    // workspace membership untouched.
    assert_eq!(f.manager.model().windows().count(), 2);
    assert_eq!(f.manager.model().focused(), focus_before);
    assert_eq!(f.manager.model().window(f.id_a).unwrap().workspace, ws_a);
    assert_eq!(f.manager.model().window(f.id_b).unwrap().workspace, ws_b);

    // Alpha shifted size-preserved into the survivor slice; beta is
    // byte-identical.
    let alpha = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((alpha.loc.x, alpha.loc.y), (1280, 0));
    assert_eq!((alpha.size.w, alpha.size.h), (800, 600));
    assert_inside_right(alpha);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), beta_before);
    assert_inside_right(f.manager.geometry(f.id_b).unwrap());
}

#[test]
fn non_primary_removal_leaves_maximized_geometry_on_new_primary() {
    let mut f = two_windows_dual();
    // Anchor the dock on the second output, then maximize there: the
    // work area is the new primary's (1920x1080 minus the panel strip).
    assert!(f.comp.state.set_primary("right"));
    assert!(f.manager.set_maximized(&mut f.comp.state, f.id_a, true));
    let maxed = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((maxed.loc.x, maxed.loc.y), (0, 32));
    assert_eq!((maxed.size.w, maxed.size.h), (1920, 1048));
    // Beta parks on the survivor so the floating path moves nothing.
    assert!(f.manager.move_window(f.id_b, 1280, 0));

    // The removed output was not primary, so managed layouts are
    // already correct and migration touches nothing.
    let moved = f.manager.migrate_output_windows(&mut f.comp.state, "left");
    assert_eq!(moved, 0, "non-primary removal re-applies nothing");
    assert!(f.comp.state.remove_output("left"));

    assert_eq!(f.manager.model().windows().count(), 2);
    assert_eq!(
        f.manager.window_layout(f.id_a),
        Some(WindowLayout::Maximized)
    );
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), maxed);
}

#[test]
fn primary_removal_reapplies_maximized_against_new_primary() {
    let mut f = two_windows_dual();
    assert!(f.manager.set_maximized(&mut f.comp.state, f.id_a, true));
    let maxed = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((maxed.size.w, maxed.size.h), (1280, 768));
    // Beta parks on the survivor so the floating path moves nothing.
    assert!(f.manager.move_window(f.id_b, 1280, 0));

    // Migration alone must NOT re-apply: the removed output is still
    // primary at migrate time, and sizing for it would go stale at
    // failover.
    let moved = f.manager.migrate_output_windows(&mut f.comp.state, "left");
    assert_eq!(moved, 0, "maximized windows wait for failover");
    assert!(f.comp.state.remove_output("left"));
    // Stale without the re-apply step: still the dead primary's area.
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), maxed);

    let applied = f.manager.reapply_derived_layouts(&mut f.comp.state);
    assert_eq!(applied, 1, "maximized window re-resolved");
    let maxed_new = f.manager.geometry(f.id_a).unwrap();
    assert_eq!((maxed_new.size.w, maxed_new.size.h), (1920, 1048));
    assert_eq!(f.manager.model().windows().count(), 2);
}

#[test]
fn migrate_unknown_output_moves_nothing() {
    let mut f = two_windows_dual();
    let geo_a = f.manager.geometry(f.id_a).unwrap();
    let geo_b = f.manager.geometry(f.id_b).unwrap();

    assert_eq!(
        f.manager.migrate_output_windows(&mut f.comp.state, "nope"),
        0
    );
    assert!(!f.comp.state.remove_output("nope"));

    // The inventory and every window are untouched.
    assert_eq!(f.manager.model().windows().count(), 2);
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), geo_a);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), geo_b);
    assert!(f.comp.state.set_primary("left"));
    assert!(f.comp.state.set_primary("right"));
}

#[test]
fn migrate_last_output_moves_nothing_and_drops_entry() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("only", None, 800, 600);
    let mut manager = comp.window_manager();
    let (conn_a, mut queue_a, mut client_a) = connect(&mut comp);
    map_toplevel(&mut comp, &conn_a, &mut queue_a, &mut client_a, "solo");
    manager.reconcile(&mut comp.state);
    sync_client(&mut comp, &conn_a, &mut queue_a, &mut client_a);
    assert_eq!(manager.model().windows().count(), 1);

    // No survivor exists, so migration moves nothing — and dropping the
    // last entry still succeeds.
    assert_eq!(manager.migrate_output_windows(&mut comp.state, "only"), 0);
    assert!(comp.state.remove_output("only"));
    assert!(
        comp.state.output_infos().is_empty(),
        "last entry drops cleanly"
    );
    // Windows are never dropped by output removal, only repositioned.
    assert_eq!(manager.model().windows().count(), 1);
}

#[test]
fn hotplug_remove_and_readd_round_trips_without_restart() {
    let mut f = two_windows_dual();
    // Beta rides the hotplugged output; removing it must carry beta
    // back to the survivor with no session restart.
    assert!(f.manager.move_window(f.id_b, 1280, 0));
    let focus_before = f.manager.model().focused();
    assert_eq!(
        f.manager.migrate_output_windows(&mut f.comp.state, "right"),
        1
    );
    assert!(f.comp.state.remove_output("right"));

    let infos = f.comp.state.output_infos();
    assert_eq!(infos.len(), 1, "removal shrinks the inventory live");
    assert_eq!(infos[0].name, "left");
    assert_eq!(f.manager.model().windows().count(), 2);
    let geo_a = f.manager.geometry(f.id_a).unwrap();
    let geo_b = f.manager.geometry(f.id_b).unwrap();

    // Re-add the same output: the inventory grows back in place and
    // every window is still where migration left it.
    f.comp.state.add_output("right", None, 1920, 1080);
    let infos = f.comp.state.output_infos();
    assert_eq!(infos.len(), 2, "re-add restores the inventory live");
    let right = infos.iter().find(|info| info.name == "right").unwrap();
    assert_eq!((right.width, right.height), (1920, 1080));
    assert!(!right.primary, "re-added entry does not steal primary");
    assert!(f.comp.state.set_primary("right"), "re-added entry is live");
    assert!(f.comp.state.set_primary("left"));

    assert_eq!(f.manager.model().windows().count(), 2);
    assert_eq!(f.manager.model().focused(), focus_before);
    assert_eq!(f.manager.geometry(f.id_a).unwrap(), geo_a);
    assert_eq!(f.manager.geometry(f.id_b).unwrap(), geo_b);
    assert_inside_right_or_left(geo_a, geo_b);
}

/// Both windows sit inside one of the two virtual slices after the
/// round-trip above (alpha never left "left", beta migrated back).
fn assert_inside_right_or_left(
    a: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    b: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
) {
    for geo in [a, b] {
        let in_left = geo.loc.x >= 0 && geo.loc.x + geo.size.w <= 1280;
        let in_right = geo.loc.x >= 1280 && geo.loc.x + geo.size.w <= 1280 + 1920;
        assert!(in_left || in_right, "window inside a live slice: {geo:?}");
    }
}

#[test]
fn real_output_global_removal_reaches_clients() {
    let mut comp = TestCompositor::new();
    // A real protocol output, mirroring `Runtime::launch`: Output plus
    // a registry global tracked in the inventory.
    let output = Output::new(
        "hot-0".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Roost".to_owned(),
            model: "Test".to_owned(),
        },
    );
    let mode = Mode {
        size: (1280, 800).into(),
        refresh: 60_000,
    };
    output.change_current_state(
        Some(mode),
        None,
        Some(Scale::Integer(1)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    let global = output.create_global::<State>(&comp.display.handle());
    comp.state.add_output("hot-0", Some(output), 1280, 800);
    comp.state.note_output_global("hot-0", global);

    // The client binds the advertised wl_output global.
    let (conn, mut queue, mut client) = connect(&mut comp);
    assert_eq!(
        client.output_names.len(),
        1,
        "client sees the hotplugged wl_output"
    );
    let advertised = client.output_names[0];
    // Barrier first: the bind above must reach the server before the
    // global goes away, or it races the removal and the server kills
    // the client (see `DisplayHandle::remove_global` docs — disable,
    // wait, then remove).
    sync_client(&mut comp, &conn, &mut queue, &mut client);

    // Removal un-advertises the global: the bound client observes the
    // withdrawal live, with no restart.
    assert!(comp.state.remove_output("hot-0"));
    pump(&mut comp, &mut queue, &mut client, |c| {
        c.removed_names.contains(&advertised)
    });
    assert!(
        client.removed_names.contains(&advertised),
        "client saw global_remove for the hotplugged output"
    );
    assert!(
        comp.state.output_infos().is_empty(),
        "real entry drops from the inventory"
    );
}
