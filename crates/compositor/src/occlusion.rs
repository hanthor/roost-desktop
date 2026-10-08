//! Occlusion culling for the scene passes (#503).
//!
//! The scene is drawn bottom to top in several passes (wallpaper, decor,
//! previews, windows and layers, tile preview, thumbnails). Without
//! culling every pass repaints the whole damage, so a region changing
//! over a stack of windows blends the wallpaper and every window beneath
//! it. Like Mutter's unobscured regions, and Smithay's
//! `OutputDamageTracker::render_output`, the scene is walked top to
//! bottom first: each element repaints only the damage not already
//! covered by an opaque element above it, and an element with nothing
//! left is skipped.
//!
//! Opacity comes only from `Element::opaque_regions`. Smithay derives it
//! from the client's `wl_surface.set_opaque_region` or a buffer format
//! without alpha, and reports nothing for an element drawn with alpha
//! below one, so shadows, rounded corners, fading and translucent
//! windows never hide what is under them.

use smithay::backend::renderer::element::{Element, RenderElement};
use smithay::backend::renderer::Renderer;
use smithay::utils::{Physical, Rectangle};

pub(crate) type PhysicalRect = Rectangle<i32, Physical>;

/// The damage still to be painted, walked from the topmost element down.
#[derive(Debug, Clone)]
pub(crate) struct Occlusion {
    damage: Vec<PhysicalRect>,
    /// Opaque area of the elements visited so far (all above the next).
    covered: Vec<PhysicalRect>,
    rendered: usize,
    culled: usize,
}

impl Occlusion {
    pub(crate) fn new(damage: &[PhysicalRect]) -> Self {
        Self {
            damage: damage.iter().copied().filter(|r| !r.is_empty()).collect(),
            covered: Vec::new(),
            rendered: 0,
            culled: 0,
        }
    }

    /// Visit the next element down: `geometry` in frame pixels, `opaque`
    /// relative to its top-left as `Element::opaque_regions` reports it.
    /// Returns the frame-space damage the element must repaint; empty
    /// means it is culled.
    pub(crate) fn visit(
        &mut self,
        geometry: PhysicalRect,
        opaque: impl IntoIterator<Item = PhysicalRect>,
    ) -> Vec<PhysicalRect> {
        let clipped = self
            .damage
            .iter()
            .filter_map(|d| d.intersection(geometry))
            .collect::<Vec<_>>();
        let visible = if clipped.is_empty() || self.covered.is_empty() {
            clipped
        } else {
            Rectangle::subtract_rects_many(clipped, self.covered.iter().copied())
        };
        if visible.is_empty() {
            self.culled += 1;
            return visible;
        }
        self.rendered += 1;
        // Rounding in scaled elements may overshoot by a pixel; an element
        // never hides more than it draws.
        self.covered.extend(opaque.into_iter().filter_map(|mut r| {
            r.loc += geometry.loc;
            r.intersection(geometry)
        }));
        visible
    }

    /// A solid fill (`frame.clear`) over `rects`: returns what to fill,
    /// and hides what is under it when the colour is opaque.
    pub(crate) fn visit_fill(&mut self, rects: &[PhysicalRect], opaque: bool) -> Vec<PhysicalRect> {
        let clipped = rects
            .iter()
            .flat_map(|r| self.damage.iter().filter_map(|d| d.intersection(*r)))
            .collect::<Vec<_>>();
        let visible = Rectangle::subtract_rects_many(clipped, self.covered.iter().copied());
        if opaque {
            self.covered.extend(visible.iter().copied());
        }
        visible
    }

    /// The damage nothing opaque covers: what the background clear paints.
    pub(crate) fn uncovered(&self) -> Vec<PhysicalRect> {
        Rectangle::subtract_rects_many(self.damage.iter().copied(), self.covered.iter().copied())
    }

    pub(crate) fn rendered(&self) -> usize {
        self.rendered
    }

    pub(crate) fn culled(&self) -> usize {
        self.culled
    }

    /// Plan one pass, front to back like the slice: per element, the
    /// frame-space damage it repaints (empty when culled).
    pub(crate) fn plan<E: Element>(
        &mut self,
        scale: f64,
        elements: &[E],
    ) -> Vec<Vec<PhysicalRect>> {
        elements
            .iter()
            .map(|element| {
                let geometry = element.geometry(scale.into());
                self.visit(geometry, element.opaque_regions(scale.into()))
            })
            .collect()
    }
}

/// Draw one planned pass bottom to top. Each element gets only its
/// visible damage.
pub(crate) fn draw_planned<R, E>(
    frame: &mut R::Frame<'_, '_>,
    scale: f64,
    elements: &[E],
    plan: &[Vec<PhysicalRect>],
) -> Result<(), R::Error>
where
    R: Renderer,
    E: RenderElement<R>,
{
    for (element, visible) in elements.iter().zip(plan).rev() {
        if visible.is_empty() {
            continue;
        }
        let geometry = element.geometry(scale.into());
        let damage: Vec<PhysicalRect> = visible
            .iter()
            .map(|r| {
                let mut r = *r;
                r.loc -= geometry.loc;
                r
            })
            .collect();
        // No opaque regions: GLES would draw them with blending off, and
        // on the nested (flipped) target that blanked the wrong pixels.
        element.draw(frame, element.src(), geometry, &damage, &[])?;
    }
    Ok(())
}

/// What one drawn frame did: elements repainted and culled, and for each
/// scene element (front to back) whether any of it was drawn.
#[derive(Debug, Clone, Default)]
pub(crate) struct Drawn {
    pub rendered: usize,
    pub culled: usize,
    pub scene_visible: Vec<bool>,
}

/// One output's last drawn frame, published in the compositor state
/// (`frame_cost`) so CI can see overdraw without a profiler.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FrameCost {
    pub rendered: usize,
    pub culled: usize,
    pub damage_rects: usize,
    pub damage_area: i64,
    pub output_area: i64,
    /// Windows with at least one element drawn.
    pub drawn_windows: Vec<u64>,
    /// Windows in the scene with every element culled.
    pub culled_windows: Vec<u64>,
}

impl FrameCost {
    /// `owners` lines up with `drawn.scene_visible` (the scene elements,
    /// front to back); `None` marks a layer or lock surface.
    pub(crate) fn new(
        drawn: &Drawn,
        owners: &[Option<u64>],
        damage: &[PhysicalRect],
        output: smithay::utils::Size<i32, Physical>,
    ) -> Self {
        let mut drawn_windows = Vec::new();
        let mut culled_windows = Vec::new();
        for (owner, visible) in owners.iter().zip(&drawn.scene_visible) {
            let Some(id) = *owner else { continue };
            if *visible {
                drawn_windows.push(id);
            } else {
                culled_windows.push(id);
            }
        }
        drawn_windows.sort_unstable();
        drawn_windows.dedup();
        culled_windows.sort_unstable();
        culled_windows.dedup();
        // A window partly drawn (a popup over another window) is drawn.
        culled_windows.retain(|id| drawn_windows.binary_search(id).is_err());
        Self {
            rendered: drawn.rendered,
            culled: drawn.culled,
            damage_rects: damage.len(),
            damage_area: area(damage),
            output_area: i64::from(output.w.max(0)) * i64::from(output.h.max(0)),
            drawn_windows,
            culled_windows,
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "rendered": self.rendered,
            "culled": self.culled,
            "damage_rects": self.damage_rects,
            "damage_area": self.damage_area,
            "output_area": self.output_area,
            "drawn_windows": self.drawn_windows,
            "culled_windows": self.culled_windows,
        })
    }
}

/// Total area of non-overlapping rectangles.
pub(crate) fn area(rects: &[PhysicalRect]) -> i64 {
    rects
        .iter()
        .map(|r| i64::from(r.size.w.max(0)) * i64::from(r.size.h.max(0)))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::{
        solid::SolidColorRenderElement, utils::RescaleRenderElement, Id, Kind,
    };
    use smithay::backend::renderer::Color32F;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> PhysicalRect {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn solid(geometry: PhysicalRect, alpha: f32) -> SolidColorRenderElement {
        SolidColorRenderElement::new(
            Id::new(),
            geometry,
            0usize,
            Color32F::new(alpha, 0.0, 0.0, alpha),
            Kind::Unspecified,
        )
    }

    const OUTPUT: (i32, i32) = (1280, 800);

    fn full() -> PhysicalRect {
        rect(0, 0, OUTPUT.0, OUTPUT.1)
    }

    #[test]
    fn fullscreen_opaque_window_culls_everything_beneath() {
        let mut occlusion = Occlusion::new(&[full()]);
        // Front to back: fullscreen window, two stacked windows, wallpaper.
        let scene = [
            solid(full(), 1.0),
            solid(rect(100, 100, 640, 420), 1.0),
            solid(rect(300, 200, 640, 420), 1.0),
        ];
        let plan = occlusion.plan(1.0, &scene);
        assert_eq!(plan[0], vec![full()]);
        assert!(plan[1].is_empty() && plan[2].is_empty());
        let paper = occlusion.plan(1.0, &[solid(full(), 1.0)]);
        assert!(paper[0].is_empty());
        assert!(occlusion.uncovered().is_empty(), "no background clear left");
        assert_eq!((occlusion.rendered(), occlusion.culled()), (1, 3));
    }

    #[test]
    fn translucent_elements_never_hide_what_is_beneath() {
        let mut occlusion = Occlusion::new(&[full()]);
        let plan = occlusion.plan(1.0, &[solid(full(), 0.5), solid(rect(10, 10, 50, 50), 1.0)]);
        assert_eq!(plan[0], vec![full()]);
        assert_eq!(plan[1], vec![rect(10, 10, 50, 50)]);
        assert_eq!(occlusion.culled(), 0);
        // A surface element at alpha < 1 (fading, overview previews)
        // reports no opaque regions; the visitor trusts that.
        let mut occlusion = Occlusion::new(&[full()]);
        occlusion.visit(full(), std::iter::empty());
        assert_eq!(occlusion.visit(full(), std::iter::empty()), vec![full()]);
    }

    #[test]
    fn partially_covered_element_repaints_only_its_uncovered_part() {
        let mut occlusion = Occlusion::new(&[full()]);
        occlusion.visit(rect(0, 0, 640, 800), [rect(0, 0, 640, 800)]);
        let visible = occlusion.visit(rect(320, 100, 640, 400), std::iter::empty());
        assert_eq!(area(&visible), 320 * 400);
        assert!(visible.iter().all(|r| r.loc.x >= 640));
    }

    #[test]
    fn client_opaque_region_excludes_shadow_margins() {
        // A CSD window: 24px shadow all round, opaque only inside it.
        let mut occlusion = Occlusion::new(&[full()]);
        let window = rect(100, 100, 688, 468);
        occlusion.visit(window, [rect(24, 24, 640, 420)]);
        let below = occlusion.visit(window, std::iter::empty());
        assert_eq!(
            area(&below),
            688 * 468 - 640 * 420,
            "the shadow band shows through"
        );
        assert!(below.iter().all(|r| !r.overlaps(rect(124, 124, 640, 420))));
    }

    #[test]
    fn opaque_regions_are_clipped_to_the_element() {
        let mut occlusion = Occlusion::new(&[full()]);
        // A rounding overshoot claiming one pixel past the right edge.
        occlusion.visit(rect(0, 0, 100, 100), [rect(0, 0, 101, 100)]);
        assert_eq!(
            occlusion.visit(rect(100, 0, 10, 10), std::iter::empty()),
            vec![rect(100, 0, 10, 10)]
        );
    }

    #[test]
    fn only_damage_is_repainted_and_disjoint_rects_stay_separate() {
        let clock = rect(600, 0, 80, 32);
        let progress = rect(200, 700, 300, 8);
        let mut occlusion = Occlusion::new(&[clock, progress]);
        let plan = occlusion.plan(
            1.0,
            &[solid(rect(100, 300, 640, 420), 1.0), solid(full(), 1.0)],
        );
        assert_eq!(
            plan[0],
            vec![progress],
            "the window repaints only its own damage"
        );
        assert_eq!(plan[1], vec![clock]);
        assert_eq!(occlusion.culled(), 0);
        // A window entirely outside the damage is culled.
        assert!(occlusion
            .visit(rect(0, 100, 50, 50), std::iter::empty())
            .is_empty());
    }

    #[test]
    fn opaque_fills_cover_and_translucent_fills_do_not() {
        let mut occlusion = Occlusion::new(&[full()]);
        let strip = [rect(0, 0, 1280, 100)];
        assert_eq!(occlusion.visit_fill(&strip, false), strip.to_vec());
        assert_eq!(area(&occlusion.uncovered()), 1280 * 800);
        assert_eq!(occlusion.visit_fill(&strip, true), strip.to_vec());
        assert_eq!(area(&occlusion.uncovered()), 1280 * 700);
        assert!(occlusion
            .visit(rect(0, 0, 1280, 100), std::iter::empty())
            .is_empty());
    }

    #[test]
    fn frame_cost_names_windows_whose_every_element_was_culled() {
        let drawn = Drawn {
            rendered: 3,
            culled: 3,
            // Front to back: layer, fullscreen window 7, its popup, then
            // window 5 (two surfaces) and window 9 with a visible popup.
            scene_visible: vec![true, true, true, false, false, true, false],
        };
        let owners = [None, Some(7), Some(7), Some(5), Some(5), Some(9), Some(9)];
        let cost = FrameCost::new(&drawn, &owners, &[full()], OUTPUT.into());
        assert_eq!(cost.drawn_windows, vec![7, 9]);
        assert_eq!(cost.culled_windows, vec![5]);
        assert_eq!(
            (cost.damage_area, cost.output_area),
            (1280 * 800, 1280 * 800)
        );
        let json = cost.to_json();
        assert_eq!(json["culled"], 3);
        assert_eq!(json["culled_windows"], serde_json::json!([5]));
    }

    #[test]
    fn rescaled_elements_report_scaled_opaque_regions() {
        // Scroll mode draws a column stretched horizontally.
        let element = RescaleRenderElement::from_element(
            solid(rect(0, 0, 400, 300), 1.0),
            (0, 0).into(),
            (1.5, 1.0),
        );
        let mut occlusion = Occlusion::new(&[full()]);
        occlusion.plan(1.0, std::slice::from_ref(&element));
        assert!(occlusion
            .visit(rect(0, 0, 600, 300), std::iter::empty())
            .is_empty());
        assert!(!occlusion
            .visit(rect(0, 0, 601, 300), std::iter::empty())
            .is_empty());
    }
}
