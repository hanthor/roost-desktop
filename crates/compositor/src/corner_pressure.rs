//! Native pointer barriers matching GNOME Shell 51's pressure policy.
//! Absolute pointer warps never supply pressure. Geometry is logical, while
//! hit distances come from the accelerated relative libinput event.
use smithay::utils::{Logical, Point, Rectangle};
use std::collections::VecDeque;

#[derive(Debug, Default)]
pub struct CornerPressure {
    layout: Vec<(Rectangle<i32, Logical>, bool)>,
    rtl: bool,
    corners: Vec<Corner>,
}

#[derive(Debug)]
struct Corner {
    rect: Rectangle<i32, Logical>,
    events: VecDeque<(u64, f64)>,
    pressure: f64,
    triggered: bool,
}

impl CornerPressure {
    pub fn reset(&mut self) {
        self.layout.clear();
        self.corners.clear();
    }

    /// Intercept an attempted relative crossing before desktop-union clamping.
    /// Return the constrained pointer and a genuine pressure trigger position.
    pub fn motion(
        &mut self,
        from: Point<f64, Logical>,
        delta: Point<f64, Logical>,
        time_ms: u64,
        layout: &[(Rectangle<i32, Logical>, bool)],
        rtl: bool,
        enabled: bool,
    ) -> (Point<f64, Logical>, Option<Point<f64, Logical>>) {
        let mut to = from + delta;
        if !enabled
            || ![from.x, from.y, delta.x, delta.y]
                .iter()
                .all(|n| n.is_finite())
        {
            self.reset();
            return (to, None);
        }
        if self.layout != layout || self.rtl != rtl {
            self.reset();
            self.layout = layout.to_vec();
            self.rtl = rtl;
            for (index, &(rect, primary)) in layout.iter().enumerate() {
                if rect.size.w <= 0 || rect.size.h <= 0 {
                    continue;
                }
                let x = f64::from(rect.loc.x) + if rtl { f64::from(rect.size.w) } else { 0.0 };
                let beside = (
                    f64::from(rect.loc.x) + if rtl { 1.0 } else { -1.0 },
                    f64::from(rect.loc.y),
                )
                    .into();
                let above = (x, f64::from(rect.loc.y) - 1.0).into();
                if primary
                    || !layout.iter().enumerate().any(|(other_index, (other, _))| {
                        index != other_index
                            && (other.to_f64().contains(beside) || other.to_f64().contains(above))
                    })
                {
                    self.corners.push(Corner {
                        rect,
                        events: VecDeque::new(),
                        pressure: 0.0,
                        triggered: false,
                    });
                }
            }
        }
        let mut trigger = None;
        for corner in &mut self.corners {
            let left = f64::from(corner.rect.loc.x);
            let top = f64::from(corner.rect.loc.y);
            // The compositor's cursor uses the last addressable logical pixel
            // at right edges, as does clamp_to_outputs().
            let edge = if rtl {
                left + f64::from(corner.rect.size.w) - 1.0
            } else {
                left
            };
            let extent = crate::windows::ACTIVITIES_STRIP_PX;
            let x_in = |x: f64| {
                if rtl {
                    (edge - extent..=edge).contains(&x)
                } else {
                    (edge..=edge + extent).contains(&x)
                }
            };
            let y_in = |y: f64| (top..=top + extent).contains(&y);
            let outward_x = if rtl { delta.x > 0.0 } else { delta.x < 0.0 };
            let across_x = if rtl {
                from.x <= edge && to.x >= edge
            } else {
                from.x >= edge && to.x <= edge
            };
            let vertical_y = from.y + delta.y * ((edge - from.x) / delta.x);
            let horizontal_x = from.x + delta.x * ((top - from.y) / delta.y);
            let vertical = outward_x && across_x && y_in(vertical_y);
            let horizontal = delta.y < 0.0 && from.y >= top && to.y <= top && x_in(horizontal_x);
            if !vertical && !horizontal {
                let on_vertical = from.x == edge && y_in(from.y) && to.x == edge && y_in(to.y);
                let on_horizontal = from.y == top && x_in(from.x) && to.y == top && x_in(to.x);
                if !on_vertical && !on_horizontal {
                    corner.events.clear();
                    corner.pressure = 0.0;
                    corner.triggered = false;
                }
                continue;
            }
            if vertical {
                to.x = edge;
            }
            if horizontal {
                to.y = top;
            }
            if corner.triggered {
                continue;
            }
            for (hit, distance, slide) in [
                (vertical, delta.x.abs(), delta.y.abs()),
                (horizontal, delta.y.abs(), delta.x.abs()),
            ] {
                if !hit || corner.triggered {
                    continue;
                }
                if distance >= 100.0 {
                    corner.triggered = true;
                } else if slide <= distance {
                    while corner
                        .events
                        .front()
                        .is_some_and(|(time, _)| *time < time_ms.saturating_sub(1000))
                    {
                        corner.pressure -= corner.events.pop_front().unwrap().1;
                    }
                    let distance = distance.min(15.0);
                    if distance == 0.0 {
                        continue;
                    }
                    if corner.events.len() >= 16384 {
                        corner.events.clear();
                        corner.pressure = 0.0;
                        continue;
                    }
                    // Events have monotonic backend timestamps. A regressing
                    // source is rejected rather than extending old pressure.
                    if corner
                        .events
                        .back()
                        .is_some_and(|(time, _)| *time > time_ms)
                    {
                        corner.events.clear();
                        corner.pressure = 0.0;
                        continue;
                    }
                    corner.events.push_back((time_ms, distance));
                    corner.pressure += distance;
                    corner.triggered = corner.pressure >= 100.0;
                }
                if corner.triggered {
                    corner.events.clear();
                    corner.pressure = 0.0;
                    trigger = Some(if vertical {
                        (edge, vertical_y).into()
                    } else {
                        (horizontal_x, top).into()
                    });
                }
            }
        }
        (to, trigger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn layout() -> Vec<(Rectangle<i32, Logical>, bool)> {
        vec![(Rectangle::new((0, 0).into(), (1000, 700).into()), true)]
    }
    fn hit(pressure: &mut CornerPressure, distance: f64, time: u64) -> bool {
        pressure
            .motion(
                (0.0, 10.0).into(),
                (-distance, 0.0).into(),
                time,
                &layout(),
                false,
                true,
            )
            .1
            .is_some()
    }
    #[test]
    fn native_pressure_requires_real_across_motion_and_caps_ordinary_hits() {
        let mut pressure = CornerPressure::default();
        for time in 0..6 {
            assert!(!hit(&mut pressure, 50.0, time));
        }
        assert!(hit(&mut pressure, 50.0, 6));
        assert!(!hit(&mut pressure, 500.0, 7), "must leave before rearming");
    }
    #[test]
    fn one_large_native_hit_bypasses_cap_and_slide_filter() {
        let mut pressure = CornerPressure::default();
        let (_, trigger) = pressure.motion(
            (0.0, 10.0).into(),
            (-100.0, 200.0).into(),
            0,
            &layout(),
            false,
            true,
        );
        assert!(trigger.is_some());
    }
    #[test]
    fn native_sliding_and_zero_motion_supply_no_pressure() {
        let mut pressure = CornerPressure::default();
        for time in 0..100 {
            assert!(pressure
                .motion(
                    (0.0, 10.0).into(),
                    (-1.0, 2.0).into(),
                    time,
                    &layout(),
                    false,
                    true
                )
                .1
                .is_none());
            assert!(!hit(&mut pressure, 0.0, time));
        }
    }
    #[test]
    fn old_pressure_expires_at_one_second() {
        let mut pressure = CornerPressure::default();
        for time in 0..6 {
            assert!(!hit(&mut pressure, 15.0, time));
        }
        assert!(!hit(&mut pressure, 15.0, 1006));
        for time in 1007..1012 {
            assert!(!hit(&mut pressure, 15.0, time));
        }
        assert!(hit(&mut pressure, 15.0, 1012));
    }
    #[test]
    fn leaving_both_barriers_rearms_and_allows_overview_toggle_again() {
        let mut pressure = CornerPressure::default();
        assert!(hit(&mut pressure, 100.0, 0));
        assert!(!hit(&mut pressure, 100.0, 1));
        pressure.motion(
            (0.0, 10.0).into(),
            (20.0, 20.0).into(),
            2,
            &layout(),
            false,
            true,
        );
        assert!(hit(&mut pressure, 100.0, 3));
    }
    #[test]
    fn disabled_policy_clears_partial_pressure() {
        let mut pressure = CornerPressure::default();
        for time in 0..6 {
            assert!(!hit(&mut pressure, 15.0, time));
        }
        pressure.motion(
            (0.0, 10.0).into(),
            (-100.0, 0.0).into(),
            7,
            &layout(),
            false,
            false,
        );
        assert!(!hit(&mut pressure, 15.0, 8));
    }
    #[test]
    fn primary_barrier_intercepts_crossing_before_output_union_clamp() {
        let mut pressure = CornerPressure::default();
        let outputs = vec![
            (Rectangle::new((0, 0).into(), (1000, 700).into()), true),
            (Rectangle::new((-1000, 0).into(), (1000, 700).into()), false),
        ];
        let (position, _) = pressure.motion(
            (2.0, 10.0).into(),
            (-10.0, 0.0).into(),
            0,
            &outputs,
            false,
            true,
        );
        assert_eq!(
            position,
            (0.0, 10.0).into(),
            "must not leak into adjacent output"
        );
    }
    #[test]
    fn covered_secondary_corner_has_no_barrier() {
        let mut pressure = CornerPressure::default();
        let outputs = vec![
            (Rectangle::new((0, 0).into(), (1000, 700).into()), true),
            (Rectangle::new((1000, 0).into(), (1000, 700).into()), false),
        ];
        let (position, trigger) = pressure.motion(
            (1002.0, 10.0).into(),
            (-200.0, 0.0).into(),
            0,
            &outputs,
            false,
            true,
        );
        assert_eq!(position, (802.0, 10.0).into());
        assert!(trigger.is_none());
    }
    #[test]
    fn rtl_barrier_uses_real_outward_motion() {
        let mut pressure = CornerPressure::default();
        let (position, trigger) = pressure.motion(
            (998.0, 10.0).into(),
            (100.0, 0.0).into(),
            0,
            &layout(),
            true,
            true,
        );
        assert_eq!(position, (999.0, 10.0).into());
        assert!(trigger.is_some());
    }
    #[test]
    fn layout_change_clears_pressure_and_uses_negative_logical_origin() {
        let mut pressure = CornerPressure::default();
        for time in 0..6 {
            assert!(!hit(&mut pressure, 15.0, time));
        }
        let outputs = vec![(Rectangle::new((-500, -200).into(), (500, 350).into()), true)];
        let (position, trigger) = pressure.motion(
            (-500.0, -190.0).into(),
            (-15.0, 0.0).into(),
            6,
            &outputs,
            false,
            true,
        );
        assert_eq!(position, (-500.0, -190.0).into());
        assert!(trigger.is_none());
    }
}
