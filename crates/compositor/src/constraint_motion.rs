//! Exact committed rectangular regions for pointer constraints.
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::compositor::{RectangleKind, RegionAttributes};

pub(crate) const MAX_RECTANGLES: usize = 512;
const MAX_MEMBERSHIP_WORK: usize = 65_536;

pub(crate) struct EffectiveRegion {
    pub extent: Rectangle<i32, Logical>,
    pub over_budget: bool,
    pub input: Option<RegionAttributes>,
    pub constraint: Option<RegionAttributes>,
}

fn in_rect(rect: Rectangle<i32, Logical>, point: Point<f64, Logical>) -> bool {
    point.x >= f64::from(rect.loc.x)
        && point.y >= f64::from(rect.loc.y)
        && point.x < f64::from(rect.loc.x) + f64::from(rect.size.w)
        && point.y < f64::from(rect.loc.y) + f64::from(rect.size.h)
}

fn in_region(region: &RegionAttributes, point: Point<f64, Logical>) -> bool {
    let mut inside = false;
    for (kind, rect) in &region.rects {
        if in_rect(*rect, point) {
            inside = matches!(kind, RectangleKind::Add);
        }
    }
    inside
}

fn toward(value: f64, target: f64) -> f64 {
    if value == target {
        value
    } else if value == 0.0 {
        f64::from_bits(1).copysign(target)
    } else if (target > value) == (value > 0.0) {
        f64::from_bits(value.to_bits() + 1)
    } else {
        f64::from_bits(value.to_bits() - 1)
    }
}

impl EffectiveRegion {
    /// Combine a bounded surface snapshot with the constraint while its
    /// Smithay guard is held, without re-entering the surface-state mutex.
    pub fn with_constraint(mut self, constraint: Option<&RegionAttributes>) -> Self {
        let total = 1usize
            .saturating_add(self.input.as_ref().map_or(0, |region| region.rects.len()))
            .saturating_add(constraint.map_or(0, |region| region.rects.len()));
        self.over_budget |= total > MAX_RECTANGLES;
        self.constraint = if self.over_budget {
            None
        } else {
            constraint.cloned()
        };
        self
    }

    pub fn bounded(&self) -> bool {
        !self.over_budget
            && 1 + self.input.as_ref().map_or(0, |region| region.rects.len())
                + self
                    .constraint
                    .as_ref()
                    .map_or(0, |region| region.rects.len())
                <= MAX_RECTANGLES
    }

    fn rectangles(&self) -> impl Iterator<Item = Rectangle<i32, Logical>> + '_ {
        std::iter::once(self.extent).chain(
            [&self.input, &self.constraint]
                .into_iter()
                .filter_map(Option::as_ref)
                .flat_map(|region| region.rects.iter().map(|(_, rect)| *rect)),
        )
    }

    pub fn contains(&self, point: Point<f64, Logical>) -> bool {
        !self.over_budget
            && point.x.is_finite()
            && point.y.is_finite()
            && in_rect(self.extent, point)
            && [&self.input, &self.constraint].into_iter().all(|region| {
                region
                    .as_ref()
                    .is_none_or(|region| in_region(region, point))
            })
    }

    /// Stop at the first forbidden interval, even if the destination is
    /// in another allowed component. Work exhaustion freezes the pointer.
    fn segment(
        &self,
        start: Point<f64, Logical>,
        end: Point<f64, Logical>,
        work: &mut usize,
    ) -> Option<Point<f64, Logical>> {
        let delta = end - start;
        let mut crossings = vec![0.0, 1.0];
        for rect in self.rectangles() {
            for (position, change, low, high) in [
                (
                    start.x,
                    delta.x,
                    f64::from(rect.loc.x),
                    f64::from(rect.loc.x) + f64::from(rect.size.w),
                ),
                (
                    start.y,
                    delta.y,
                    f64::from(rect.loc.y),
                    f64::from(rect.loc.y) + f64::from(rect.size.h),
                ),
            ] {
                if change != 0.0 {
                    for edge in [low, high] {
                        let time = (edge - position) / change;
                        if time > 0.0 && time < 1.0 {
                            crossings.push(time);
                        }
                    }
                }
            }
        }
        crossings.sort_by(f64::total_cmp);
        crossings.dedup();
        for interval in crossings.windows(2) {
            *work = work.checked_add(self.rectangles().count())?;
            if *work > MAX_MEMBERSHIP_WORK {
                return None;
            }
            let midpoint = start + delta.upscale(interval[0] + (interval[1] - interval[0]) / 2.0);
            if !self.contains(midpoint) {
                let edge = start + delta.upscale(interval[0]);
                if self.contains(edge) {
                    return Some(edge);
                }
                // One representable coordinate step toward the known
                // interior, not a geometric epsilon or acceptance tolerance.
                let inside = (toward(edge.x, start.x), toward(edge.y, start.y)).into();
                return Some(if self.contains(inside) { inside } else { start });
            }
        }
        if self.contains(end) {
            Some(end)
        } else {
            let inside = (toward(end.x, start.x), toward(end.y, start.y)).into();
            Some(if self.contains(inside) { inside } else { start })
        }
    }

    pub fn confine(
        &self,
        start: Point<f64, Logical>,
        end: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        if !self.bounded() || !self.contains(start) || !end.x.is_finite() || !end.y.is_finite() {
            return start;
        }
        // Preserve the existing whole-window rectangle behavior.
        if self.input.is_none() && self.constraint.is_none() {
            return (
                end.x.clamp(
                    f64::from(self.extent.loc.x),
                    f64::from(self.extent.loc.x) + f64::from(self.extent.size.w) - 1.0,
                ),
                end.y.clamp(
                    f64::from(self.extent.loc.y),
                    f64::from(self.extent.loc.y) + f64::from(self.extent.size.h) - 1.0,
                ),
            )
                .into();
        }
        let mut work = 0;
        let Some(edge) = self.segment(start, end, &mut work) else {
            return start;
        };
        if edge == end {
            return end;
        }
        // Continue tangential movement without jumping through a hole.
        let Some(horizontal) = self.segment(edge, (end.x, edge.y).into(), &mut work) else {
            return start;
        };
        let Some(vertical) = self.segment(edge, (edge.x, end.y).into(), &mut work) else {
            return start;
        };
        let distance = |point: Point<f64, Logical>| (end.x - point.x).hypot(end.y - point.y);
        if distance(horizontal) < distance(vertical) {
            horizontal
        } else {
            vertical
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }
    fn region(rects: Vec<(RectangleKind, Rectangle<i32, Logical>)>) -> EffectiveRegion {
        EffectiveRegion {
            extent: rect(-100, -100, 300, 300),
            over_budget: false,
            input: None,
            constraint: Some(RegionAttributes { rects }),
        }
    }
    #[test]
    fn subtracted_hole_cannot_be_crossed_to_an_allowed_destination() {
        let region = region(vec![
            (RectangleKind::Add, rect(0, 0, 100, 100)),
            (RectangleKind::Subtract, rect(20, 0, 10, 100)),
        ]);
        let position = region.confine((10.0, 10.0).into(), (80.0, 10.0).into());
        assert!(position.x < 20.0 && position.x > 19.999999999999);
        assert!(region.contains(position));
    }
    #[test]
    fn exact_order_can_add_back_a_subtracted_region() {
        let region = region(vec![
            (RectangleKind::Add, rect(0, 0, 100, 100)),
            (RectangleKind::Subtract, rect(20, 0, 10, 100)),
            (RectangleKind::Add, rect(20, 0, 10, 100)),
        ]);
        assert_eq!(
            region.confine((10.0, 10.0).into(), (80.0, 10.0).into()),
            (80.0, 10.0).into()
        );
    }
    #[test]
    fn disjoint_components_and_empty_region_do_not_broaden() {
        let region = region(vec![
            (RectangleKind::Add, rect(0, 0, 10, 10)),
            (RectangleKind::Add, rect(20, 0, 10, 10)),
        ]);
        assert!(region.confine((5.0, 5.0).into(), (25.0, 5.0).into()).x < 10.0);
        assert!(!super::tests::region(vec![]).contains((5.0, 5.0).into()));
    }
    #[test]
    fn negative_subpixel_half_open_boundary_stays_inside() {
        let region = region(vec![(RectangleKind::Add, rect(-10, -10, 10, 10))]);
        let position = region.confine((-0.25, -0.5).into(), (0.25, 0.5).into());
        assert!(position.x < 0.0 && position.y < 0.0);
        assert!(region.contains(position));
        assert!(!region.contains((0.0, -0.5).into()));
    }
    #[test]
    fn boundary_slide_preserves_tangential_motion() {
        let region = region(vec![(RectangleKind::Add, rect(0, 0, 100, 100))]);
        let position = region.confine((10.0, 10.0).into(), (-20.0, 70.0).into());
        assert_eq!(position, (0.0, 70.0).into());
    }
    #[test]
    fn input_region_intersects_constraint_and_extent() {
        let mut region = region(vec![(RectangleKind::Add, rect(0, 0, 100, 100))]);
        region.input = Some(RegionAttributes {
            rects: vec![(RectangleKind::Add, rect(5, 5, 10, 10))],
        });
        assert!(!region.contains((4.9, 10.0).into()));
        assert!(region.contains((5.0, 5.0).into()));
        assert!(region.confine((10.0, 10.0).into(), (80.0, 10.0).into()).x < 15.0);
    }
    #[test]
    fn oversized_region_work_fails_closed() {
        let region = region(vec![
            (RectangleKind::Add, rect(0, 0, 100, 100));
            MAX_RECTANGLES
        ]);
        let start = (10.0, 10.0).into();
        assert!(!region.bounded());
        assert_eq!(region.confine(start, (80.0, 10.0).into()), start);
    }
    #[test]
    fn separated_surface_snapshot_keeps_combined_budget_closed() {
        let mut snapshot = region(vec![]);
        snapshot.constraint = None;
        snapshot.input = Some(RegionAttributes {
            rects: vec![(RectangleKind::Add, rect(0, 0, 100, 100)); MAX_RECTANGLES - 2],
        });
        let constraint = RegionAttributes {
            rects: vec![(RectangleKind::Add, rect(0, 0, 100, 100)); 2],
        };
        let combined = snapshot.with_constraint(Some(&constraint));
        assert!(!combined.bounded());
        assert!(
            combined.constraint.is_none(),
            "oversized constraint is never cloned"
        );
        let start = (10.0, 10.0).into();
        assert_eq!(combined.confine(start, (80.0, 10.0).into()), start);
    }
}
