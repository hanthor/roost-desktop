//! Compare the complete native scene before acquiring a scanout buffer.
//! Dirty scenes still repaint the complete buffer: this cache never assumes
//! an older swapchain buffer contains the last submitted image.

use smithay::backend::renderer::{
    element::{Element, Id},
    utils::CommitCounter,
    Color32F,
};
use smithay::utils::{Buffer, Physical, Rectangle, Transform};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ElementSignature {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
    source: Rectangle<f64, Buffer>,
    transform: Transform,
    alpha: f32,
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
        }
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
}
