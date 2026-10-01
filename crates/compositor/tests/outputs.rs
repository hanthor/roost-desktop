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
