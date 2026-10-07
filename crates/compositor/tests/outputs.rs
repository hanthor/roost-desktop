//! Task 1 output-inventory tests.
//!
//! The compositor tracks one inventory entry per connected output
//! (size plus primary flag); the first entry is primary and
//! single-output sessions hold exactly one entry. Virtual entries
//! carry `None` handles. Conventions follow `layer.rs`: a
//! `TestCompositor` fixture, plain asserts, no sleeps. All code here
//! is original.

use roost_compositor::TestCompositor;
use smithay::utils::Size;

#[test]
fn two_virtual_outputs_report_own_sizes_and_primary_flags() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);

    // First entry registered is primary with its own size; virtual
    // entries carry no protocol handle.
    assert_eq!(
        comp.state.primary_size(),
        Size::from((1280, 800)),
        "first entry is primary"
    );
    assert!(
        comp.state.primary_output().is_none(),
        "virtual entries use None handles"
    );

    // Each entry reports its own size once anchored primary.
    assert!(comp.state.set_primary("right"));
    assert_eq!(comp.state.primary_size(), Size::from((1920, 1080)));
    assert!(comp.state.set_primary("left"));
    assert_eq!(comp.state.primary_size(), Size::from((1280, 800)));
}

#[test]
fn remove_unknown_output_returns_false_and_keeps_inventory() {
    let mut comp = TestCompositor::new();
    assert!(
        !comp.state.remove_output("nope"),
        "empty inventory drops nothing"
    );

    comp.state.add_output("left", None, 1280, 800);
    assert!(!comp.state.remove_output("nope"));
    assert_eq!(
        comp.state.primary_size(),
        Size::from((1280, 800)),
        "failed removal leaves the inventory untouched"
    );
    assert!(comp.state.set_primary("left"));
}

#[test]
fn removing_secondary_output_keeps_primary() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);

    assert!(comp.state.remove_output("right"));
    assert_eq!(comp.state.primary_size(), Size::from((1280, 800)));
    assert!(comp.state.set_primary("left"));
    assert!(!comp.state.set_primary("right"), "dropped entry is gone");
}

#[test]
fn removing_primary_fails_over_to_first_survivor() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);
    assert!(comp.state.set_primary("right"));

    assert!(comp.state.remove_output("right"));
    assert_eq!(
        comp.state.primary_size(),
        Size::from((1280, 800)),
        "first survivor takes over primary"
    );
    assert!(comp.state.set_primary("left"));
    assert!(!comp.state.set_primary("right"), "dropped entry is gone");
}

#[test]
fn add_output_upserts_by_name_without_growing_inventory() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);

    // Same name updates the entry in place: still primary, new size.
    comp.state.add_output("left", None, 800, 600);
    assert_eq!(comp.state.primary_size(), Size::from((800, 600)));
    // The sibling entry is untouched by the upsert.
    assert!(comp.state.set_primary("right"));
    assert_eq!(comp.state.primary_size(), Size::from((1920, 1080)));
    assert!(comp.state.set_primary("left"));
    assert_eq!(comp.state.primary_size(), Size::from((800, 600)));

    // One removal drops the name entirely: no duplicate lingers.
    assert!(comp.state.remove_output("left"));
    assert!(
        !comp.state.set_primary("left"),
        "upsert left exactly one entry behind"
    );
}

#[test]
fn set_primary_moves_anchor_and_rejects_unknown() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);

    assert!(!comp.state.set_primary("nope"));
    assert_eq!(
        comp.state.primary_size(),
        Size::from((1280, 800)),
        "failed switch leaves the anchor untouched"
    );
    assert!(comp.state.set_primary("right"));
    assert_eq!(comp.state.primary_size(), Size::from((1920, 1080)));
}

#[test]
fn set_output_size_registers_virtual_primary_on_empty() {
    let mut comp = TestCompositor::new();
    assert_eq!(
        comp.state.primary_size(),
        Size::from((0, 0)),
        "empty inventory reads back zero sizes"
    );

    comp.state.set_output_size(1280, 800);
    assert_eq!(comp.state.primary_size(), Size::from((1280, 800)));
    assert!(
        comp.state.primary_output().is_none(),
        "backward-compat entry is virtual"
    );
}

#[test]
fn output_infos_lists_primary_first_with_own_sizes() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);

    let infos = comp.state.output_infos();
    assert_eq!(infos.len(), 2, "one record per inventory entry");
    assert_eq!(infos[0].name, "left");
    assert!(infos[0].primary, "first entry registered is primary");
    assert_eq!((infos[0].width, infos[0].height), (1280, 800));
    assert_eq!(infos[1].name, "right");
    assert!(!infos[1].primary);
    assert_eq!((infos[1].width, infos[1].height), (1920, 1080));

    // Primary switch reorders the records: the dock anchor leads.
    assert!(comp.state.set_primary("right"));
    let infos = comp.state.output_infos();
    assert_eq!(infos[0].name, "right");
    assert!(infos[0].primary);
    assert_eq!((infos[0].width, infos[0].height), (1920, 1080));
    assert_eq!(infos[1].name, "left");
    assert!(!infos[1].primary);
}

#[test]
fn size_and_loc_follow_bound_output_with_auto_tile_offset() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);

    assert_eq!(
        comp.state.size_for_output(Some("left")),
        Size::from((1280, 800))
    );
    assert_eq!(
        comp.state.size_for_output(Some("right")),
        Size::from((1920, 1080))
    );
    assert_eq!(comp.state.loc_for_output(Some("left")), (0, 0));
    assert_eq!(
        comp.state.loc_for_output(Some("right")),
        (1280, 0),
        "later entries tile right of the existing row"
    );
}

#[test]
fn unknown_or_unbound_output_falls_back_to_primary() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 800);
    comp.state.add_output("right", None, 1920, 1080);

    // Unknown names and unbound surfaces behave like today's global
    // surface: they arrange against the primary entry.
    assert_eq!(
        comp.state.size_for_output(Some("nope")),
        Size::from((1280, 800))
    );
    assert_eq!(comp.state.size_for_output(None), Size::from((1280, 800)));
    assert_eq!(comp.state.loc_for_output(Some("nope")), (0, 0));
    assert_eq!(comp.state.loc_for_output(None), (0, 0));

    // The fallback tracks the anchor when primary moves.
    assert!(comp.state.set_primary("right"));
    assert_eq!(
        comp.state.size_for_output(Some("nope")),
        Size::from((1920, 1080))
    );
    assert_eq!(
        comp.state.loc_for_output(Some("nope")),
        (1280, 0),
        "fallback follows the primary slice"
    );
}

#[test]
fn set_output_size_updates_primary_in_place_without_adding_entry() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("roost-0", None, 800, 600);

    comp.state.set_output_size(1280, 800);
    assert_eq!(
        comp.state.primary_size(),
        Size::from((1280, 800)),
        "single-output path updates the one entry"
    );

    // Still one entry: removing the name empties the inventory, so no
    // second entry was pushed beside it.
    assert!(comp.state.remove_output("roost-0"));
    assert_eq!(comp.state.primary_size(), Size::from((0, 0)));
    assert!(!comp.state.set_primary("roost-0"));
}

#[test]
fn activities_follow_live_primary_geometry_and_output_disconnect() {
    use roost_compositor::windows::{ManagerInput, TriggerAction as A, TriggerState};
    let mut comp = TestCompositor::new();
    comp.state.add_output("left", None, 1280, 720);
    comp.state.add_output("right", None, 1280, 720);
    comp.state.set_output_location("left", (-1280, -200));
    comp.state.set_output_location("right", (0, 0));
    let mut triggers = TriggerState::default();
    let button = ManagerInput::Button {
        button: 0x110,
        pressed: true,
        time: 0,
    };
    let click = |comp: &TestCompositor, triggers: &mut TriggerState, x, y| {
        comp.state
            .overview_trigger_action(triggers, &button, false, (x, y).into())
    };
    assert_eq!(click(&comp, &mut triggers, -1278.0, -190.0), A::Toggle);
    for (x, y) in [
        (-1281.0, -190.0),
        (-1278.0, -201.0),
        (-1184.0, -190.0),
        (-1278.0, -168.0),
        (2.0, 10.0),
        (f64::NAN, -190.0),
        (-1278.0, f64::INFINITY),
    ] {
        assert_eq!(click(&comp, &mut triggers, x, y), A::None);
    }
    assert!(comp.state.set_primary("right"));
    assert_eq!(click(&comp, &mut triggers, 2.0, 10.0), A::Toggle);
    assert_eq!(click(&comp, &mut triggers, -1278.0, -190.0), A::None);
    assert!(comp.state.remove_output("right"));
    assert_eq!(click(&comp, &mut triggers, -1278.0, -190.0), A::Toggle);
    assert_eq!(click(&comp, &mut triggers, 2.0, 10.0), A::None);
    assert!(comp.state.remove_output("left"));
    assert_eq!(click(&comp, &mut triggers, -1278.0, -190.0), A::None);
    assert_eq!(click(&comp, &mut triggers, 2.0, 10.0), A::None);
}

#[test]
fn hot_corners_use_exposed_secondary_corners_and_live_gnome_policy() {
    use roost_compositor::windows::{ManagerInput, TriggerAction as A, TriggerState};
    let mut comp = TestCompositor::new();
    comp.state.add_output("primary", None, 1280, 720);
    comp.state.add_output("secondary", None, 1280, 720);
    let mut triggers = TriggerState::default();
    let motion = |comp: &TestCompositor, triggers: &mut TriggerState, x, y| {
        let input = ManagerInput::Motion {
            pos: (x, y).into(),
            time: 0,
        };
        comp.state
            .overview_trigger_action(triggers, &input, false, (x, y).into())
    };
    assert_eq!(motion(&comp, &mut triggers, 2.0, 2.0), A::Open);
    assert_eq!(
        motion(&comp, &mut triggers, 1282.0, 2.0),
        A::None,
        "secondary has an output immediately to its left"
    );
    comp.state.set_output_location("secondary", (0, 720));
    assert_eq!(
        motion(&comp, &mut triggers, 2.0, 722.0),
        A::None,
        "secondary has an output immediately above"
    );
    comp.state.set_output_location("secondary", (1280, 720));
    assert_eq!(
        motion(&comp, &mut triggers, 1282.0, 722.0),
        A::Open,
        "diagonal contact leaves both approach points exposed"
    );
    comp.state.set_output_location("secondary", (-1280, -200));
    assert_eq!(motion(&comp, &mut triggers, -1278.0, -198.0), A::Open);
    for (x, y) in [
        (-1281.0, -198.0),
        (-1278.0, -201.0),
        (-1272.0, -198.0),
        (-1278.0, -192.0),
        (f64::NEG_INFINITY, -198.0),
        (-1278.0, f64::NAN),
    ] {
        assert_eq!(motion(&comp, &mut triggers, x, y), A::None);
    }
    triggers.set_hot_corner(false);
    assert_eq!(motion(&comp, &mut triggers, -1278.0, -198.0), A::None);
    triggers.set_hot_corner(true);
    assert_eq!(motion(&comp, &mut triggers, -1278.0, -198.0), A::Open);
    comp.state.set_output_location("secondary", (1280, 0));
    assert!(comp.state.set_primary("secondary"));
    assert_eq!(
        motion(&comp, &mut triggers, 1282.0, 2.0),
        A::Open,
        "GNOME keeps the primary corner even beside another output"
    );
    assert!(comp.state.remove_output("secondary"));
    assert_eq!(motion(&comp, &mut triggers, 1282.0, 2.0), A::None);
}

#[test]
fn desktop_size_tracks_layout_scale_mirrors_and_disconnects() {
    let mut comp = TestCompositor::new();
    assert_eq!(comp.state.desktop_size(), (0, 0));
    comp.state.add_output("primary", None, 1280, 800);
    comp.state.add_output("left", None, 1024, 768);
    comp.state.set_output_location("left", (-1024, -200));
    assert_eq!(comp.state.desktop_size(), (2304, 1000));
    assert!(comp.state.set_primary("left"));
    assert_eq!(comp.state.desktop_size(), (2304, 1000));
    // The backend publishes its newly scaled logical geometry to this inventory.
    comp.state.add_output("left", None, 512, 384);
    assert_eq!(comp.state.desktop_size(), (2304, 1000));
    comp.state.set_output_location("left", (-512, 0));
    assert_eq!(comp.state.desktop_size(), (1792, 800));
    // Mirrored geometry is a union, not the sum of monitor widths.
    comp.state.set_output_location("left", (0, 0));
    assert_eq!(comp.state.desktop_size(), (1280, 800));
    // Translation of the entire layout does not alter its dimensions.
    comp.state.set_output_location("left", (-700, -500));
    comp.state.set_output_location("primary", (-700, -500));
    assert_eq!(comp.state.desktop_size(), (1280, 800));
    comp.state.remove_output("primary");
    assert_eq!(comp.state.desktop_size(), (512, 384));
    comp.state.remove_output("left");
    assert_eq!(comp.state.desktop_size(), (0, 0));
}

#[test]
fn desktop_size_bounds_extreme_coordinates_and_ignores_empty_outputs() {
    let mut comp = TestCompositor::new();
    comp.state.add_output("a", None, 1, 1);
    comp.state.set_output_location("a", (i32::MIN, i32::MIN));
    comp.state.add_output("b", None, i32::MAX, i32::MAX);
    comp.state.set_output_location("b", (i32::MAX, i32::MAX));
    assert_eq!(comp.state.desktop_size(), (i32::MAX, i32::MAX));
    comp.state.remove_output("a");
    comp.state.remove_output("b");
    comp.state.add_output("disabled", None, 0, 800);
    assert_eq!(comp.state.desktop_size(), (0, 0));
}

#[test]
fn rtl_corners_and_activities_follow_live_direction_and_output_bounds() {
    use roost_compositor::windows::{ManagerInput, TriggerAction as A, TriggerState};
    let mut comp = TestCompositor::new();
    comp.state.add_output("primary", None, 1280, 720);
    comp.state.set_output_location("primary", (-1280, -200));
    let mut triggers = TriggerState::default();
    let motion = |triggers: &mut TriggerState, x, y| {
        comp.state.overview_trigger_action(
            triggers,
            &ManagerInput::Motion {
                pos: (x, y).into(),
                time: 0,
            },
            false,
            (x, y).into(),
        )
    };
    assert_eq!(motion(&mut triggers, -1278.0, -198.0), A::Open);
    triggers.set_right_to_left(true);
    assert_eq!(motion(&mut triggers, -1278.0, -198.0), A::None);
    for x in [-8.0, -0.5] {
        assert_eq!(motion(&mut triggers, x, -198.0), A::Open);
    }
    for (x, y) in [
        (-8.5, -198.0),
        (0.0, -198.0),
        (-0.5, -201.0),
        (-0.5, -192.0),
        (f64::INFINITY, -198.0),
        (-0.5, f64::NAN),
    ] {
        assert_eq!(motion(&mut triggers, x, y), A::None);
    }
    let button = ManagerInput::Button {
        button: 272,
        pressed: true,
        time: 0,
    };
    assert_eq!(
        comp.state
            .overview_trigger_action(&mut triggers, &button, false, (-0.5, -190.0).into()),
        A::Toggle
    );
    assert_eq!(
        comp.state
            .overview_trigger_action(&mut triggers, &button, false, (-1278.0, -190.0).into()),
        A::None
    );
    triggers.set_hot_corner(false);
    assert_eq!(motion(&mut triggers, -0.5, -198.0), A::None);
    triggers.set_hot_corner(true);
    assert_eq!(motion(&mut triggers, -0.5, -198.0), A::Open);
    triggers.set_right_to_left(false);
    assert_eq!(motion(&mut triggers, -0.5, -198.0), A::None);
    assert_eq!(motion(&mut triggers, -1278.0, -198.0), A::Open);
}

#[test]
fn rtl_secondary_corner_uses_gnome51_approach_probes_and_primary_override() {
    use roost_compositor::windows::{ManagerInput, TriggerAction as A, TriggerState};
    let mut comp = TestCompositor::new();
    comp.state.add_output("primary", None, 1280, 720);
    comp.state.add_output("secondary", None, 1280, 720);
    comp.state.set_output_location("secondary", (0, 720));
    let mut triggers = TriggerState::default();
    triggers.set_right_to_left(true);
    let motion = |comp: &TestCompositor, triggers: &mut TriggerState| {
        comp.state.overview_trigger_action(
            triggers,
            &ManagerInput::Motion {
                pos: (1279.5, 722.0).into(),
                time: 0,
            },
            false,
            (1279.5, 722.0).into(),
        )
    };
    // GNOME probes above at the right boundary itself, rather than right-1.
    assert_eq!(motion(&comp, &mut triggers), A::Open);
    comp.state.set_output_location("primary", (1, 0));
    assert_eq!(motion(&comp, &mut triggers), A::None);
    assert!(comp.state.set_primary("secondary"));
    assert_eq!(motion(&comp, &mut triggers), A::Open);
    assert!(comp.state.remove_output("secondary"));
    assert_eq!(motion(&comp, &mut triggers), A::None);
}
