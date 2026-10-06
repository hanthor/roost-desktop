//! Mixed-DPI (#59): a window gets the scale of the output it overlaps
//! most, as niri sends it.

use roost_compositor::TestCompositor;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::Rectangle;

fn output(name: &str, scale: f64) -> Output {
    let out = Output::new(
        name.to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: name.into(),
        },
    );
    out.change_current_state(
        Some(Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        }),
        None,
        Some(Scale::Fractional(scale)),
        None,
    );
    out
}

#[test]
fn windows_take_the_scale_of_the_output_they_overlap_most() {
    let mut comp = TestCompositor::new();
    // A 150 percent laptop on the left, a 100 percent monitor to its right.
    comp.state
        .add_output("eDP-1", Some(output("eDP-1", 1.5)), 1280, 720);
    comp.state
        .add_output("HDMI-A-1", Some(output("HDMI-A-1", 1.0)), 1920, 1080);
    let on_laptop = Rectangle::new((100, 100).into(), (400, 300).into());
    let mostly_monitor = Rectangle::new((1200, 100).into(), (600, 300).into());
    let nowhere = Rectangle::new((-5000, -5000).into(), (10, 10).into());
    assert_eq!(comp.state.scale_for(on_laptop).fractional_scale(), 1.5);
    assert_eq!(comp.state.scale_for(mostly_monitor).fractional_scale(), 1.0);
    assert_eq!(
        comp.state.scale_for(nowhere).fractional_scale(),
        1.5,
        "off-screen falls back to the primary"
    );

    // monitors.xml can stack them vertically instead.
    comp.state.set_output_location("HDMI-A-1", (0, 720));
    let below = Rectangle::new((100, 800).into(), (400, 300).into());
    assert_eq!(comp.state.scale_for(below).fractional_scale(), 1.0);
}

#[test]
fn new_windows_start_at_the_scale_of_the_output_under_the_pointer() {
    use roost_compositor::windows::WindowManager;
    let mut comp = TestCompositor::new();
    comp.state
        .add_output("eDP-1", Some(output("eDP-1", 1.5)), 1280, 720);
    comp.state
        .add_output("HDMI-A-1", Some(output("HDMI-A-1", 1.0)), 1920, 1080);
    let mut manager = WindowManager::new(&mut comp.state);
    manager.pointer_motion(&mut comp.state, (1400.0, 100.0).into(), 0);
    assert_eq!(comp.state.initial_window_scale(), 1.0);
    manager.pointer_motion(&mut comp.state, (100.0, 100.0).into(), 1);
    assert_eq!(comp.state.initial_window_scale(), 1.5);
    // Vertical monitor arrangements use the same logical coordinates.
    comp.state.set_output_location("HDMI-A-1", (0, 720));
    manager.pointer_motion(&mut comp.state, (100.0, 800.0).into(), 2);
    assert_eq!(comp.state.initial_window_scale(), 1.0);
    manager.pointer_motion(&mut comp.state, (-100.0, -100.0).into(), 3);
    assert_eq!(comp.state.initial_window_scale(), 1.5);
}

mod first_event {
    use super::*;
    use std::os::unix::net::UnixStream;
    use wayland_client::{
        protocol::{
            wl_compositor::WlCompositor, wl_registry, wl_registry::WlRegistry,
            wl_surface::WlSurface,
        },
        Connection, Dispatch, QueueHandle,
    };
    use wayland_protocols::wp::fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    };

    #[derive(Default)]
    struct Client {
        compositor: Option<WlCompositor>,
        manager: Option<WpFractionalScaleManagerV1>,
        scales: Vec<u32>,
    }

    impl Dispatch<WlRegistry, ()> for Client {
        fn event(
            state: &mut Self,
            registry: &WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            {
                match interface.as_str() {
                    "wl_compositor" => {
                        state.compositor = Some(registry.bind(name, version, qh, ()))
                    }
                    "wp_fractional_scale_manager_v1" => {
                        state.manager = Some(registry.bind(name, 1, qh, ()))
                    }
                    _ => {}
                }
            }
        }
    }
    impl Dispatch<WpFractionalScaleV1, ()> for Client {
        fn event(
            state: &mut Self,
            _: &WpFractionalScaleV1,
            event: wp_fractional_scale_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
                state.scales.push(scale);
            }
        }
    }
    wayland_client::delegate_noop!(Client: ignore WlCompositor);
    wayland_client::delegate_noop!(Client: ignore WlSurface);
    wayland_client::delegate_noop!(Client: ignore WpFractionalScaleManagerV1);

    #[test]
    fn first_fractional_scale_event_uses_the_target_output() {
        use roost_compositor::windows::WindowManager;
        let mut comp = TestCompositor::new();
        comp.state
            .add_output("left", Some(output("left", 1.0)), 1280, 720);
        comp.state
            .add_output("right", Some(output("right", 1.75)), 1280, 720);
        let mut windows = WindowManager::new(&mut comp.state);
        windows.pointer_motion(&mut comp.state, (1500.0, 100.0).into(), 0);
        let (server, stream) = UnixStream::pair().unwrap();
        comp.add_client(server);
        let connection = Connection::from_socket(stream).unwrap();
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        connection.display().get_registry(&qh, ());
        let mut client = Client::default();
        let mut pump = |client: &mut Client| {
            for _ in 0..10 {
                queue.flush().unwrap();
                comp.pump();
                if let Some(guard) = queue.prepare_read() {
                    let _ = guard.read();
                }
                queue.dispatch_pending(client).unwrap();
            }
        };
        pump(&mut client);
        let surface = client.compositor.as_ref().unwrap().create_surface(&qh, ());
        let _scale = client
            .manager
            .as_ref()
            .unwrap()
            .get_fractional_scale(&surface, &qh, ());
        pump(&mut client);
        assert_eq!(
            client.scales,
            vec![210],
            "first event is 175%, before any buffer or placement"
        );
    }
}

#[test]
fn hot_corner_regions_use_logical_pixels_at_fractional_output_scales() {
    use roost_compositor::windows::{ManagerInput, TriggerAction as A, TriggerState};
    let mut comp = TestCompositor::new();
    comp.state
        .add_output("laptop", Some(output("laptop", 1.5)), 1280, 720);
    comp.state
        .add_output("external", Some(output("external", 2.0)), 960, 540);
    comp.state.set_output_location("external", (-960, 0));
    let mut triggers = TriggerState::default();
    for (x, expected) in [
        (-960.0, A::Open),
        (-952.5, A::Open),
        (-952.0, A::None),
        (-961.0, A::None),
    ] {
        let input = ManagerInput::Motion {
            pos: (x, 2.0).into(),
            time: 0,
        };
        assert_eq!(
            comp.state
                .overview_trigger_action(&mut triggers, &input, false, (x, 2.0).into()),
            expected
        );
    }
}
