//! Actual headless Wayland enter/leave lifecycle; no GPU/frame qualification.
use roost_compositor::{State, TestCompositor};
use smithay::{
    backend::renderer::element::Id,
    output::{Output, PhysicalProperties, Subpixel},
};
use std::os::unix::net::UnixStream;
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_output::WlOutput,
        wl_registry::{self, WlRegistry},
        wl_surface::{self, WlSurface},
    },
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};

#[derive(Default)]
struct Client {
    compositor: Option<WlCompositor>,
    outputs: Vec<(u32, WlOutput)>,
    events: Vec<(&'static str, u32)>,
}
impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        s: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => s.compositor = Some(registry.bind(name, version.min(6), qh, ())),
                "wl_output" => s
                    .outputs
                    .push((name, registry.bind(name, version.min(4), qh, ()))),
                _ => (),
            },
            wl_registry::Event::GlobalRemove { name } => s.events.push(("remove", name)),
            _ => (),
        }
    }
}
impl Dispatch<WlSurface, ()> for Client {
    fn event(
        s: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_surface::Event::Enter { output } => {
                s.events.push(("enter", output.id().protocol_id()))
            }
            wl_surface::Event::Leave { output } => {
                s.events.push(("leave", output.id().protocol_id()))
            }
            _ => (),
        }
    }
}
wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlOutput);
fn output(name: &str) -> Output {
    Output::new(
        name.to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: name.into(),
        },
    )
}
fn advertise(comp: &mut TestCompositor, name: &str) -> Output {
    let out = output(name);
    let global = out.create_global::<State>(&comp.display.handle());
    comp.state.add_output(name, Some(out.clone()), 128, 128);
    comp.state.note_output_global(name, global);
    out
}
fn pump(comp: &mut TestCompositor, queue: &mut EventQueue<Client>, client: &mut Client) {
    for _ in 0..10 {
        queue.flush().unwrap();
        comp.pump();
        if let Some(guard) = queue.prepare_read() {
            let _ = guard.read();
        }
        queue.dispatch_pending(client).unwrap();
    }
}
#[test]
fn actual_surface_wire_membership_is_per_output_and_replacement_leaves_before_withdrawal() {
    let mut comp = TestCompositor::new();
    let left = advertise(&mut comp, "left");
    let right = advertise(&mut comp, "right");
    comp.state.set_output_location("left", (-128, 17));
    let (server, stream) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    let principal = comp.add_client(server);
    let connection = Connection::from_socket(stream).unwrap();
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut client = Client::default();
    pump(&mut comp, &mut queue, &mut client);
    let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
    pump(&mut comp, &mut queue, &mut client);
    let server_surface = principal
        .object_from_protocol_id::<wayland_server::protocol::wl_surface::WlSurface>(
            &comp.display.handle(),
            surface.id().protocol_id(),
        )
        .unwrap();
    let id = Id::from_wayland_resource(&server_surface);
    assert!(
        client.events.is_empty(),
        "advertisement alone never creates membership"
    );
    let left_wire = client.outputs[0].1.id().protocol_id();
    let right_wire = client.outputs[1].1.id().protocol_id();
    let left_global = client.outputs[0].0;
    comp.state
        .presented_surface_membership(&left, &[id.clone(), id.clone()]);
    comp.state
        .presented_surface_membership(&left, std::slice::from_ref(&id));
    comp.state
        .presented_surface_membership(&right, std::slice::from_ref(&id));
    pump(&mut comp, &mut queue, &mut client);
    assert_eq!(
        client.events,
        vec![("enter", left_wire), ("enter", right_wire)]
    );
    client.events.clear();
    assert!(comp.state.set_primary("right"));
    assert!(comp.state.set_primary("right"));
    assert!(!comp.state.set_primary("missing"));
    let entries = comp.state.output_entries();
    assert_eq!(entries[0].1, left);
    assert_eq!(entries[1].1, right);
    assert!(!entries[0].3);
    assert!(entries[1].3);
    pump(&mut comp, &mut queue, &mut client);
    assert!(
        client.events.is_empty(),
        "primary flags never recreate outputs or membership"
    );
    assert!(comp.state.set_primary("left"));

    comp.state.presented_surface_membership(&left, &[]);
    pump(&mut comp, &mut queue, &mut client);
    assert_eq!(client.events.last(), Some(&("leave", left_wire)));
    client.events.clear();
    comp.state
        .presented_surface_membership(&left, std::slice::from_ref(&id));
    pump(&mut comp, &mut queue, &mut client);
    client.events.clear();
    let replacement = output("left");
    assert!(comp
        .state
        .replace_output_protocol("left", replacement.clone(), 256, 128));
    pump(&mut comp, &mut queue, &mut client);
    assert_eq!(
        client.events,
        vec![("leave", left_wire), ("remove", left_global)]
    );
    let entries = comp.state.output_entries();
    assert_eq!(entries[0].2, (-128, 17));
    assert!(entries[0].3);
    assert_eq!(client.outputs.len(), 3);
    let replacement_wire = client.outputs[2].1.id().protocol_id();
    client.events.clear();
    comp.state
        .presented_surface_membership(&replacement, std::slice::from_ref(&id));
    pump(&mut comp, &mut queue, &mut client);
    assert_eq!(client.events, vec![("enter", replacement_wire)]);
    client.events.clear();
    assert!(comp.state.remove_output("right"));
    pump(&mut comp, &mut queue, &mut client);
    assert_eq!(client.events[0], ("leave", right_wire));
    assert_eq!(client.events[1], ("remove", client.outputs[1].0));
    client.events.clear();
    surface.destroy();
    pump(&mut comp, &mut queue, &mut client);
    comp.state.presented_surface_membership(&replacement, &[]);
    pump(&mut comp, &mut queue, &mut client);
    assert!(
        client.events.is_empty(),
        "destroyed original surface receives no stale leave"
    );
}

#[test]
fn reconciled_primary_follows_backend_after_repeated_reorder_and_removal() {
    let mut comp = TestCompositor::new();
    let _a = advertise(&mut comp, "A");
    let _b = advertise(&mut comp, "B");
    let _c = advertise(&mut comp, "C");
    // Apply C then B rotates the backend to B,C,A, but State's stable
    // protocol storage remains A,B,C. Removing B must select actual C.
    assert!(comp.state.set_primary("C"));
    assert!(comp.state.set_primary("B"));
    assert_eq!(
        comp.state
            .output_entries()
            .iter()
            .map(|(name, ..)| name.as_str())
            .collect::<Vec<_>>(),
        ["A", "B", "C"]
    );
    assert!(comp
        .state
        .select_reconciled_primary(&["C".into(), "A".into()]));
    assert_eq!(comp.state.primary_output_name().as_deref(), Some("C"));
    assert!(comp.state.remove_output("B"));
    assert_eq!(comp.state.primary_output_name().as_deref(), Some("C"));
    assert!(comp
        .state
        .output_entries()
        .iter()
        .all(|(name, _, _, primary)| *primary == (name == "C")));
    assert!(!comp.state.select_reconciled_primary(&[]));
    assert!(!comp.state.select_reconciled_primary(&["absent".into()]));
    assert_eq!(comp.state.primary_output_name().as_deref(), Some("C"));
}
