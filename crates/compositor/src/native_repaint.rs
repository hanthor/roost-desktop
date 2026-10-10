//! Compare the complete native scene before acquiring a scanout buffer.
//! Buffer-age history accounts for every intervening submitted frame; global
//! drawing changes and reset buffers conservatively require a complete paint.

use smithay::backend::renderer::{
    damage::OutputDamageTracker,
    element::{Element, Id},
    utils::CommitCounter,
    Color32F,
};
use smithay::output::OutputNoMode;
use smithay::utils::{Buffer, Physical, Rectangle, Transform};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ElementSignature {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
    source: Rectangle<f64, Buffer>,
    transform: Transform,
    alpha: f32,
    opaque: Vec<Rectangle<i32, Physical>>,
}

impl ElementSignature {
    pub(crate) fn capture(element: &impl Element, scale: f64) -> Self {
        Self {
            id: element.id().clone(),
            commit: element.current_commit(),
            geometry: element.geometry(scale.into()),
            source: element.src(),
            transform: element.transform(),
            alpha: element.alpha(),
            opaque: element
                .opaque_regions(scale.into())
                .iter()
                .copied()
                .collect(),
        }
    }
}

// Captured geometry is already physical. Default damage_since marks the
// complete changed element; no opacity is claimed, so hidden damage is retained.
impl Element for ElementSignature {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.source
    }
    fn geometry(&self, _scale: smithay::utils::Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }
    fn transform(&self) -> Transform {
        self.transform
    }
    fn alpha(&self) -> f32 {
        self.alpha
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameSignature {
    pub size: (i32, i32),
    pub location: (i32, i32),
    pub scale: f64,
    pub locked: bool,
    pub background: Color32F,
    pub blank_alpha: f32,
    pub accent: [f32; 3],
    pub pointer: Option<(f64, f64)>,
    pub decor: Vec<(Color32F, Vec<Rectangle<i32, Physical>>)>,
    pub above: usize,
    // Separate passes preserve both ordering and which scale each pass uses.
    pub groups: Vec<Vec<ElementSignature>>,
}

impl FrameSignature {
    /// Inputs outside the element damage history, or a different pass layout,
    /// must repaint the complete acquired buffer.
    pub(crate) fn same_global_drawing(&self, old: &Self) -> bool {
        self.size == old.size
            && self.location == old.location
            && self.scale == old.scale
            && self.locked == old.locked
            && self.background == old.background
            && self.blank_alpha == old.blank_alpha
            && self.accent == old.accent
            && self.pointer == old.pointer
            && self.decor == old.decor
            && self.above == old.above
            && self.groups.len() == old.groups.len()
            && self.groups.iter().zip(&old.groups).all(|(new, old)| {
                new.len() == old.len()
                    && new
                        .iter()
                        .zip(old)
                        .all(|(new, old)| new.id == old.id && new.opaque == old.opaque)
            })
    }
}

/// More separate rectangles than this cost more in per-rect setup than
/// they save in fill; past it the damage collapses to its bounds.
pub(crate) const MAX_DAMAGE_RECTS: usize = 16;

/// The buffer-age damage as a set of rectangles (#503): a clock tick in
/// the bar and a progress bar in a window repaint two small areas, not
/// the bounding box spanning the output.
pub(crate) fn damage_region(
    tracker: &mut OutputDamageTracker,
    age: usize,
    signature: &FrameSignature,
) -> Result<Vec<Rectangle<i32, Physical>>, OutputNoMode> {
    let elements: Vec<_> = signature.groups.iter().flatten().collect();
    let (damage, _) = tracker.damage_output(age, &elements)?;
    // A timing-only request still needs an actual page flip. A minimal repaint
    // submits that real frame rather than manufacturing presentation feedback.
    let rects = coalesce(damage.map(|rects| rects.to_vec()).unwrap_or_default());
    Ok(if rects.is_empty() {
        vec![Rectangle::new((0, 0).into(), (1, 1).into())]
    } else {
        rects
    })
}

/// Drop empty rectangles and merge overlapping ones, so every pixel is
/// painted once; too many left collapse to their bounding box.
pub(crate) fn coalesce(rects: Vec<Rectangle<i32, Physical>>) -> Vec<Rectangle<i32, Physical>> {
    let mut out: Vec<Rectangle<i32, Physical>> = Vec::with_capacity(rects.len());
    for mut rect in rects.into_iter().filter(|r| !r.is_empty()) {
        // Merging can make a rectangle overlap ones already kept.
        while let Some(at) = out.iter().position(|o| o.overlaps(rect)) {
            rect = rect.merge(out.swap_remove(at));
        }
        out.push(rect);
    }
    if out.len() > MAX_DAMAGE_RECTS {
        out = out
            .into_iter()
            .reduce(Rectangle::merge)
            .into_iter()
            .collect();
    }
    out
}

/// The plane damage (KMS `FB_DAMAGE_CLIPS`) submitted with a repainted
/// buffer: exactly the region repainted into it. That is the buffer-age
/// damage since this buffer was last scanned out, so it covers both the
/// change from the previous frame (drivers copying into one shadow
/// scanout) and everything stale in this buffer (drivers uploading each
/// buffer separately, such as virtio-gpu). Without it the kernel treats
/// the whole plane as damaged and copies or uploads every pixel.
pub(crate) fn scanout_damage(
    repainted: &[Rectangle<i32, Physical>],
) -> Vec<Rectangle<i32, Physical>> {
    repainted.to_vec()
}

/// Timing requests need an actual submitted frame even without pixel damage.
pub(crate) fn needs_repaint(
    previous: Option<&FrameSignature>,
    next: &FrameSignature,
    timing: bool,
) -> bool {
    timing || previous != Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::{solid::SolidColorRenderElement, Kind};

    fn scene() -> FrameSignature {
        FrameSignature {
            size: (1280, 800),
            location: (0, 0),
            scale: 1.0,
            locked: false,
            background: Color32F::BLACK,
            blank_alpha: 0.0,
            accent: [0.0; 3],
            pointer: Some((100.0, 200.0)),
            decor: vec![],
            above: 0,
            groups: vec![],
        }
    }

    #[test]
    fn unchanged_scene_sleeps_but_first_frame_and_timing_requests_render() {
        let next = scene();
        assert!(needs_repaint(None, &next, false));
        assert!(!needs_repaint(Some(&next), &next, false));
        assert!(needs_repaint(Some(&next), &next, true));
    }

    #[test]
    fn lock_fade_cursor_output_and_decor_changes_render_immediately() {
        let before = scene();
        let mut variants = vec![before.clone(); 9];
        variants[0].locked = true;
        variants[1].blank_alpha = 1.0;
        variants[2].pointer = Some((101.0, 200.0));
        variants[3].size = (800, 600);
        variants[4].scale = 1.25;
        variants[5].location = (1280, 0);
        variants[6].background = Color32F::new(1.0, 1.0, 1.0, 1.0);
        variants[7].accent = [1.0; 3];
        variants[8].decor = vec![(
            Color32F::new(1.0, 1.0, 1.0, 1.0),
            vec![Rectangle::from_size((40, 30).into())],
        )];
        for next in variants {
            assert!(needs_repaint(Some(&before), &next, false));
        }
    }

    #[test]
    fn buffer_commits_and_geometry_are_part_of_the_image() {
        let id = Id::new();
        let geo = Rectangle::from_size((40, 30).into());
        let element = |commit, geometry| {
            SolidColorRenderElement::new(
                id.clone(),
                geometry,
                commit,
                Color32F::new(1.0, 1.0, 1.0, 1.0),
                Kind::Unspecified,
            )
        };
        let mut before = scene();
        before.groups = vec![vec![ElementSignature::capture(&element(0usize, geo), 1.0)]];
        for changed in [
            ElementSignature::capture(&element(1usize, geo), 1.0),
            ElementSignature::capture(&element(0usize, Rectangle::from_size((41, 30).into())), 1.0),
        ] {
            let mut next = before.clone();
            next.groups[0][0] = changed;
            assert!(needs_repaint(Some(&before), &next, false));
        }
        let mut removed = before.clone();
        removed.groups.clear();
        assert!(needs_repaint(Some(&before), &removed, false));
    }

    #[test]
    fn asynchronously_ready_background_repaints_an_idle_scene() {
        // The native path polls wallpaper/card workers before capturing this
        // signature. An absent card becoming ready adds its new buffer ID to
        // the background pass even without an input event or client commit.
        let mut pending = scene();
        pending.groups = vec![vec![]];
        assert!(!needs_repaint(Some(&pending), &pending, false));
        let mut ready = pending.clone();
        ready.groups[0].push(ElementSignature::capture(
            &SolidColorRenderElement::new(
                Id::new(),
                Rectangle::from_size((40, 30).into()),
                0usize,
                Color32F::BLACK,
                Kind::Unspecified,
            ),
            1.0,
        ));
        assert!(needs_repaint(Some(&pending), &ready, false));
        assert!(!ready.same_global_drawing(&pending));
        assert!(!needs_repaint(Some(&ready), &ready, false));
    }

    #[test]
    fn overlapping_elements_and_render_passes_preserve_order() {
        let element = || {
            ElementSignature::capture(
                &SolidColorRenderElement::new(
                    Id::new(),
                    Rectangle::from_size((40, 30).into()),
                    0usize,
                    Color32F::BLACK,
                    Kind::Unspecified,
                ),
                1.0,
            )
        };
        let mut before = scene();
        before.groups = vec![vec![element(), element()], vec![]];
        let mut reordered = before.clone();
        reordered.groups[0].reverse();
        assert!(needs_repaint(Some(&before), &reordered, false));
        let mut other_pass = before.clone();
        let moved = other_pass.groups[0].pop().unwrap();
        other_pass.groups[1].push(moved);
        assert!(needs_repaint(Some(&before), &other_pass, false));
        let mut partition = before.clone();
        partition.above = 1;
        assert!(needs_repaint(Some(&before), &partition, false));
    }

    #[test]
    fn same_buffer_with_changed_crop_transform_or_opacity_repaints() {
        let element = SolidColorRenderElement::new(
            Id::new(),
            Rectangle::from_size((40, 30).into()),
            0usize,
            Color32F::BLACK,
            Kind::Unspecified,
        );
        let mut before = scene();
        before.groups = vec![vec![ElementSignature::capture(&element, 1.0)]];
        let mut variants = vec![before.clone(); 3];
        variants[0].groups[0][0].source.loc.x += 1.0;
        variants[1].groups[0][0].transform = Transform::Flipped180;
        variants[2].groups[0][0].alpha = 0.5;
        for next in variants {
            assert!(needs_repaint(Some(&before), &next, false));
        }
    }
    fn moving_scene(id: &Id, x: i32, commit: usize) -> FrameSignature {
        let mut next = scene();
        next.size = (32, 32);
        next.groups = vec![vec![ElementSignature::capture(
            &SolidColorRenderElement::new(
                id.clone(),
                Rectangle::new((x, 0).into(), (8, 8).into()),
                commit,
                Color32F::new(1.0, 0.0, 0.0, 1.0),
                Kind::Unspecified,
            ),
            1.0,
        )]];
        next
    }

    fn contains(rects: &[Rectangle<i32, Physical>], x: i32, y: i32) -> bool {
        rects.iter().any(|r| r.contains((x, y)))
    }

    #[test]
    fn recycled_buffer_repaints_damage_from_intervening_frames() {
        let id = Id::new();
        let mut tracker = OutputDamageTracker::new((32, 32), 1.0, Transform::Normal);
        let full = vec![Rectangle::from_size((32, 32).into())];
        assert_eq!(
            damage_region(&mut tracker, 0, &moving_scene(&id, 0, 0)).unwrap(),
            full
        );
        damage_region(&mut tracker, 1, &moving_scene(&id, 8, 1)).unwrap();
        let damage = damage_region(&mut tracker, 2, &moving_scene(&id, 16, 2)).unwrap();
        assert!(contains(&damage, 1, 1));
        assert!(contains(&damage, 23, 7));
        assert!(damage.iter().all(|r| r.size.w < 32 && r.size.h < 32));
        assert_eq!(
            damage_region(&mut tracker, 100, &moving_scene(&id, 16, 2)).unwrap(),
            full
        );
    }

    #[test]
    fn timing_only_frame_and_removed_elements_keep_correct_damage() {
        let id = Id::new();
        let mut tracker = OutputDamageTracker::new((32, 32), 1.0, Transform::Normal);
        let mut next = moving_scene(&id, 0, 0);
        damage_region(&mut tracker, 0, &next).unwrap();
        assert_eq!(
            damage_region(&mut tracker, 1, &next).unwrap(),
            vec![Rectangle::new((0, 0).into(), (1, 1).into())]
        );
        next.groups.iter_mut().for_each(Vec::clear);
        assert!(contains(
            &damage_region(&mut tracker, 1, &next).unwrap(),
            7,
            7
        ));
    }

    #[test]
    fn scanout_damage_is_the_repainted_buffer_age_region() {
        let id = Id::new();
        let mut tracker = OutputDamageTracker::new((32, 32), 1.0, Transform::Normal);
        let full = damage_region(&mut tracker, 0, &moving_scene(&id, 0, 0)).unwrap();
        assert_eq!(
            scanout_damage(&full),
            vec![Rectangle::from_size((32, 32).into())]
        );
        damage_region(&mut tracker, 1, &moving_scene(&id, 8, 1)).unwrap();
        // Two frames back: covers both moves, not the whole plane.
        let aged = damage_region(&mut tracker, 2, &moving_scene(&id, 16, 2)).unwrap();
        let clips = scanout_damage(&aged);
        assert_eq!(clips, aged);
        assert!(contains(&clips, 1, 1) && contains(&clips, 23, 7));
        assert!(clips.iter().all(|r| r.size.w < 32));
    }

    /// Two small elements far apart: a clock in the bar and a progress
    /// bar near the bottom.
    fn scattered_scene(clock: &Id, bar: &Id, commit: usize) -> FrameSignature {
        let mut next = scene();
        next.size = (1280, 800);
        let element = |id: &Id, geo| {
            ElementSignature::capture(
                &SolidColorRenderElement::new(
                    id.clone(),
                    geo,
                    commit,
                    Color32F::new(1.0, 0.0, 0.0, 1.0),
                    Kind::Unspecified,
                ),
                1.0,
            )
        };
        next.groups = vec![vec![
            element(clock, Rectangle::new((600, 0).into(), (80, 32).into())),
            element(bar, Rectangle::new((200, 700).into(), (300, 8).into())),
        ]];
        next
    }

    #[test]
    fn scattered_damage_stays_a_set_of_small_rects() {
        let (clock, bar) = (Id::new(), Id::new());
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        damage_region(&mut tracker, 0, &scattered_scene(&clock, &bar, 0)).unwrap();
        let damage = damage_region(&mut tracker, 1, &scattered_scene(&clock, &bar, 1)).unwrap();
        assert_eq!(damage.len(), 2, "{damage:?}");
        let area: i32 = damage.iter().map(|r| r.size.w * r.size.h).sum();
        assert_eq!(
            area,
            80 * 32 + 300 * 8,
            "not the bounding box spanning the output"
        );
    }

    #[test]
    fn coalesce_merges_overlaps_and_caps_the_rect_count() {
        let r = |x, y, w, h| Rectangle::<i32, Physical>::new((x, y).into(), (w, h).into());
        // Overlapping rects merge (each pixel painted once); a merge that
        // creates a new overlap merges again; empties drop.
        let merged = coalesce(vec![
            r(0, 0, 10, 10),
            r(20, 0, 10, 10),
            r(5, 0, 20, 5),
            r(50, 50, 0, 4),
        ]);
        assert_eq!(merged, vec![r(0, 0, 30, 10)]);
        let disjoint = coalesce(vec![r(0, 0, 4, 4), r(100, 100, 4, 4)]);
        assert_eq!(disjoint.len(), 2);
        let many: Vec<_> = (0..=MAX_DAMAGE_RECTS as i32)
            .map(|i| r(i * 10, 0, 4, 4))
            .collect();
        assert_eq!(
            coalesce(many),
            vec![r(0, 0, MAX_DAMAGE_RECTS as i32 * 10 + 4, 4)]
        );
    }

    #[test]
    fn global_drawing_pass_order_and_opacity_changes_reset_buffer_history() {
        let id = Id::new();
        let before = moving_scene(&id, 0, 0);
        assert!(before.same_global_drawing(&moving_scene(&id, 8, 1)));
        let mut variants = vec![before.clone(); 5];
        variants[0].locked = true;
        variants[1].pointer = None;
        variants[2].scale = 2.0;
        variants[3].groups[0][0].opaque.clear();
        variants[3].groups[0][0]
            .opaque
            .push(Rectangle::from_size((1, 1).into()));
        variants[4].groups[0][0].id = Id::new();
        for next in variants {
            assert!(!next.same_global_drawing(&before));
        }
    }
}
