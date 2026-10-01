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
