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

pub(crate) fn damage_region(
    tracker: &mut OutputDamageTracker,
    age: usize,
    signature: &FrameSignature,
) -> Result<Rectangle<i32, Physical>, OutputNoMode> {
    let elements: Vec<_> = signature.groups.iter().flatten().collect();
    let (damage, _) = tracker.damage_output(age, &elements)?;
    // A timing-only request still needs an actual page flip. A minimal repaint
    // submits that real frame rather than manufacturing presentation feedback.
    Ok(damage
        .and_then(|rects| rects.iter().copied().reduce(Rectangle::merge))
        .unwrap_or_else(|| Rectangle::new((0, 0).into(), (1, 1).into())))
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

    #[test]
    fn recycled_buffer_repaints_damage_from_intervening_frames() {
        let id = Id::new();
        let mut tracker = OutputDamageTracker::new((32, 32), 1.0, Transform::Normal);
        let full = Rectangle::from_size((32, 32).into());
        assert_eq!(
            damage_region(&mut tracker, 0, &moving_scene(&id, 0, 0)).unwrap(),
            full
        );
        damage_region(&mut tracker, 1, &moving_scene(&id, 8, 1)).unwrap();
        let damage = damage_region(&mut tracker, 2, &moving_scene(&id, 16, 2)).unwrap();
        assert!(damage.contains((1, 1)));
        assert!(damage.contains((23, 7)));
        assert!(damage.size.w < 32 && damage.size.h < 32);
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
            Rectangle::new((0, 0).into(), (1, 1).into())
        );
        next.groups.iter_mut().for_each(Vec::clear);
        assert!(damage_region(&mut tracker, 1, &next)
            .unwrap()
            .contains((7, 7)));
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
