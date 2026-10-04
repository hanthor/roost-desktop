//! Deterministic geometry motion shared by tile previews and strip columns.
use crate::spring::Spring;
use smithay::utils::{Logical, Rectangle};

pub type Rect = Rectangle<i32, Logical>;

/// GNOME tile-preview's 200 ms ease-out-quad.
#[derive(Debug, Clone, Copy)]
pub struct EaseRect {
    pub from: Rect,
    pub to: Rect,
}
impl EaseRect {
    pub fn value_at(&self, seconds: f64, enabled: bool) -> Rect {
        let p = if enabled {
            crate::overview::ease_out_quad(seconds / 0.2)
        } else {
            1.0
        };
        interpolate(self.from, self.to, p)
    }
}

fn interpolate(from: Rect, to: Rect, p: f64) -> Rect {
    let l = |a: i32, b: i32| (f64::from(a) + f64::from(b - a) * p).round() as i32;
    Rect::new(
        (l(from.loc.x, to.loc.x), l(from.loc.y, to.loc.y)).into(),
        (l(from.size.w, to.size.w), l(from.size.h, to.size.h)).into(),
    )
}

/// niri's default window-movement and window-resize springs (800/1/0.0001).
#[derive(Debug, Clone, Copy)]
pub struct ColumnSpring {
    pub to: Rect,
    x: Spring,
    width: Spring,
    elapsed: f64,
}
impl ColumnSpring {
    pub fn new(from: Rect, to: Rect) -> Self {
        Self {
            to,
            x: Spring::view_movement(f64::from(from.loc.x), f64::from(to.loc.x), 0.0),
            width: Spring::view_movement(f64::from(from.size.w), f64::from(to.size.w), 0.0),
            elapsed: 0.0,
        }
    }
    pub fn retarget(&mut self, to: Rect) {
        if to == self.to {
            return;
        }
        self.x = Spring::view_movement(
            self.x.value_at(self.elapsed),
            f64::from(to.loc.x),
            self.x.velocity_at(self.elapsed),
        );
        self.width = Spring::view_movement(
            self.width.value_at(self.elapsed),
            f64::from(to.size.w),
            self.width.velocity_at(self.elapsed),
        );
        self.to = to;
        self.elapsed = 0.0;
    }
    pub fn step(&mut self, dt: f64, enabled: bool) -> bool {
        self.elapsed += dt.max(0.0);
        if !enabled || (self.x.done_at(self.elapsed) && self.width.done_at(self.elapsed)) {
            *self = Self::new(self.to, self.to);
            false
        } else {
            true
        }
    }
    pub fn value(&self) -> Rect {
        Rect::new(
            (self.x.value_at(self.elapsed).round() as i32, self.to.loc.y).into(),
            (
                self.width.value_at(self.elapsed).round().max(1.0) as i32,
                self.to.size.h,
            )
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: i32, width: i32) -> Rect {
        Rect::new((x, 32).into(), (width, 700).into())
    }
    #[test]
    fn tile_preview_eases_from_the_window_in_200_ms() {
        let animation = EaseRect {
            from: rect(300, 600),
            to: rect(0, 640),
        };
        assert_eq!(animation.value_at(0.0, true), animation.from);
        assert_eq!(animation.value_at(0.1, true), rect(75, 630));
        assert_eq!(animation.value_at(0.2, true), animation.to);
        assert_eq!(animation.value_at(0.0, false), animation.to);
    }
    #[test]
    fn strip_columns_spring_width_and_position_and_snap_when_disabled() {
        let mut animation = ColumnSpring::new(rect(0, 600), rect(632, 900));
        assert!(animation.step(0.1, true));
        let shown = animation.value();
        assert!(shown.loc.x > 0 && shown.loc.x < 632);
        assert!(shown.size.w > 600 && shown.size.w < 900);
        animation.retarget(rect(300, 700));
        assert_eq!(animation.value(), shown);
        assert!(!animation.step(0.0, false));
        assert_eq!(animation.value(), rect(300, 700));
        let mut animation = ColumnSpring::new(rect(0, 600), rect(632, 900));
        assert!(!animation.step(0.5, true));
        assert_eq!(animation.value(), animation.to);
    }
}
