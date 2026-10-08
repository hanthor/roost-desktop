//! GNOME 51's Activities workspace dots (`panel.js` `WorkspaceDot`):
//! each dot's expansion is one minus its distance from the active
//! workspace, so the wide pill slides between dots as the workspace
//! changes (250 ms ease-out-cubic, the workspace switch). An expanded dot
//! is 8px times the width multiplier wide, fully opaque and full size;
//! a collapsed one is 8px, half opaque and drawn at 0.75. Dots for added
//! workspaces scale in and dots for removed ones scale out over 500 ms
//! ease-out-cubic.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

/// The `.workspace-dot` size.
const DOT: f64 = 8.0;
/// `INACTIVE_WORKSPACE_DOT_SCALE`.
const INACTIVE_SCALE: f64 = 0.75;
/// The expansion follows the workspace switch.
pub const MOVE_MS: f64 = 250.0;
/// `scaleIn` / `scaleOutAndDestroy`.
pub const SCALE_MS: f64 = 500.0;

pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    t * t * t + 1.0
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// `_updateExpansion`'s width multiplier for `n` dots.
pub fn width_multiplier(n: usize) -> f64 {
    match n {
        0..=2 => 3.625,
        3..=5 => 3.25,
        _ => 2.75,
    }
}

/// One dot as drawn: its layout width and the visible pill inside it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DotLook {
    pub slot: f64,
    pub width: f64,
    pub height: f64,
    pub opacity: f64,
}

/// A dot at `expansion` (0 collapsed, 1 the active pill), scaled by
/// `scale` (its scale-in or scale-out).
pub fn look(expansion: f64, scale: f64, multiplier: f64) -> DotLook {
    let e = expansion.clamp(0.0, 1.0);
    let slot = DOT * lerp(1.0, multiplier, e);
    let s = lerp(INACTIVE_SCALE, 1.0, e) * scale;
    DotLook {
        slot,
        width: slot * s,
        height: DOT * s,
        opacity: lerp(0.5, 1.0, e),
    }
}

#[derive(Debug, Clone, Copy)]
struct Dot {
    from: f64,
    to: f64,
    start: f64,
}

impl Dot {
    fn scale(&self, now: f64) -> f64 {
        lerp(
            self.from,
            self.to,
            ease_out_cubic((now - self.start) / SCALE_MS),
        )
    }
    fn removing(&self) -> bool {
        self.to == 0.0
    }
    fn settled(&self, now: f64) -> bool {
        now - self.start >= SCALE_MS
    }
}

/// The dots' animation state, in milliseconds of a monotonic clock.
#[derive(Debug, Default, Clone)]
pub struct Dots {
    dots: Vec<Dot>,
    /// The active position: eased from `.0` to `.1` from `.2`.
    position: (f64, f64, f64),
}

impl Dots {
    fn position(&self, now: f64) -> f64 {
        let (from, to, start) = self.position;
        lerp(from, to, ease_out_cubic((now - start) / MOVE_MS))
    }

    /// Show `count` workspaces with `active` current.
    pub fn set(&mut self, count: usize, active: usize, now: f64, enabled: bool) {
        // The first workspaces seen are simply there.
        let first = self.dots.is_empty();
        let live: Vec<usize> = (0..self.dots.len())
            .filter(|i| !self.dots[*i].removing())
            .collect();
        if live.len() < count {
            for _ in live.len()..count {
                let from = if enabled && !first { 0.0 } else { 1.0 };
                self.dots.push(Dot {
                    from,
                    to: 1.0,
                    start: now,
                });
            }
        } else {
            // GNOME drops the last dots.
            for i in live.into_iter().skip(count) {
                let dot = &mut self.dots[i];
                *dot = Dot {
                    from: dot.scale(now),
                    to: 0.0,
                    start: now,
                };
            }
        }
        let target = active as f64;
        if first {
            self.position = (target, target, f64::NEG_INFINITY);
        } else if self.position.1 != target {
            self.position = (self.position(now), target, now);
        }
        if !enabled {
            self.finish();
        }
    }

    /// Jump every motion to its end.
    pub fn finish(&mut self) {
        self.dots.retain(|dot| !dot.removing());
        self.dots.fill(Dot {
            from: 1.0,
            to: 1.0,
            start: f64::NEG_INFINITY,
        });
        self.position = (self.position.1, self.position.1, f64::NEG_INFINITY);
    }

    /// Drop dots whose scale-out finished; returns whether anything
    /// still moves.
    pub fn prune(&mut self, now: f64) -> bool {
        self.dots
            .retain(|dot| !(dot.removing() && dot.settled(now)));
        now - self.position.2 < MOVE_MS || self.dots.iter().any(|dot| !dot.settled(now))
    }

    /// Every dot as drawn at `now`.
    pub fn looks(&self, now: f64) -> Vec<DotLook> {
        let live = self.dots.iter().filter(|dot| !dot.removing()).count();
        let multiplier = width_multiplier(live);
        let position = self.position(now);
        self.dots
            .iter()
            .enumerate()
            .map(|(i, dot)| {
                let expansion = 1.0 - (i as f64 - position).abs();
                look(expansion, dot.scale(now), multiplier)
            })
            .collect()
    }
}

fn now_ms() -> f64 {
    glib::monotonic_time() as f64 / 1000.0
}

fn animations_enabled() -> bool {
    gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations())
}

/// The dots in the Activities button.
pub struct Indicator {
    row: gtk::Box,
    dots: RefCell<Dots>,
    ticking: Cell<bool>,
}

impl Indicator {
    pub fn new(row: gtk::Box) -> Rc<Self> {
        Rc::new(Self {
            row,
            dots: RefCell::default(),
            ticking: Cell::new(false),
        })
    }

    /// Show the workspaces `pills` describes (`true` the active one).
    pub fn show(self: &Rc<Self>, pills: &[bool]) {
        let active = pills.iter().position(|a| *a).unwrap_or(0);
        let now = now_ms();
        self.dots
            .borrow_mut()
            .set(pills.len(), active, now, animations_enabled());
        self.apply(now);
        if self.ticking.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        self.row.add_tick_callback(move |_, _| {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let now = now_ms();
            if !animations_enabled() {
                ui.dots.borrow_mut().finish();
            }
            let moving = ui.apply(now);
            if moving {
                glib::ControlFlow::Continue
            } else {
                ui.ticking.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    /// Lay the dots out for `now`; returns whether any still move.
    fn apply(&self, now: f64) -> bool {
        let moving = self.dots.borrow_mut().prune(now);
        let looks = self.dots.borrow().looks(now);
        let position = self.dots.borrow().position.1 as usize;
        let mut child = self.row.first_child();
        for (i, look) in looks.iter().enumerate() {
            let pill = match child.take() {
                Some(widget) => widget,
                None => {
                    let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    pill.add_css_class("ws-pill");
                    pill.set_valign(gtk::Align::Center);
                    pill.set_halign(gtk::Align::Center);
                    self.row.append(&pill);
                    pill.upcast()
                }
            };
            child = pill.next_sibling();
            if i == position {
                pill.add_css_class("active");
            } else {
                pill.remove_css_class("active");
            }
            let width = look.width.round() as i32;
            let height = look.height.round() as i32;
            let side = ((look.slot.round() as i32 - width) / 2).max(0);
            let edge = ((DOT.round() as i32 - height) / 2).max(0);
            pill.set_size_request(width, height);
            pill.set_margin_start(side);
            pill.set_margin_end(look.slot.round() as i32 - width - side);
            pill.set_margin_top(edge);
            pill.set_margin_bottom(DOT.round() as i32 - height - edge);
            pill.set_opacity(look.opacity);
            pill.set_visible(width > 0 && height > 0);
        }
        while let Some(extra) = child {
            child = extra.next_sibling();
            self.row.remove(&extra);
        }
        moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_dots_match_gnome_sizes() {
        // Two workspaces: the active pill 29px, the other a 6px dot in
        // an 8px slot at half opacity.
        let active = look(1.0, 1.0, width_multiplier(2));
        assert_eq!(
            (active.slot, active.height, active.opacity),
            (29.0, 8.0, 1.0)
        );
        let idle = look(0.0, 1.0, width_multiplier(2));
        assert_eq!(
            (idle.slot, idle.width, idle.height, idle.opacity),
            (8.0, 6.0, 6.0, 0.5)
        );
        assert_eq!(look(1.0, 1.0, width_multiplier(5)).slot, 26.0);
        assert_eq!(look(1.0, 1.0, width_multiplier(6)).slot, 22.0);
    }

    #[test]
    fn the_pill_moves_between_dots_over_250_ms() {
        let mut dots = Dots::default();
        dots.set(3, 0, 0.0, true);
        dots.finish();
        dots.set(3, 1, 1000.0, true);
        let start = dots.looks(1000.0);
        assert_eq!(start[0].opacity, 1.0);
        let mid = dots.looks(1125.0);
        assert!(mid[0].slot > 8.0 && mid[0].slot < start[0].slot);
        assert!(mid[1].slot > 8.0);
        assert!(dots.prune(1125.0));
        let end = dots.looks(1250.0);
        assert_eq!((end[0].slot, end[1].slot), (8.0, 26.0));
        assert!(!dots.prune(1250.0));
    }

    #[test]
    fn added_dots_scale_in_and_removed_dots_scale_out_over_500_ms() {
        let mut dots = Dots::default();
        dots.set(2, 0, 0.0, true);
        dots.finish();
        dots.set(3, 0, 1000.0, true);
        assert_eq!(dots.looks(1000.0)[2].width, 0.0);
        assert!(dots.looks(1250.0)[2].width > 0.0);
        assert_eq!(dots.looks(1500.0)[2].width, 6.0);
        dots.set(2, 0, 2000.0, true);
        assert_eq!(dots.looks(2000.0).len(), 3);
        assert!(dots.prune(2400.0));
        assert!(!dots.prune(2500.0));
        assert_eq!(dots.looks(2500.0).len(), 2);
    }

    #[test]
    fn without_animations_everything_lands_at_once() {
        let mut dots = Dots::default();
        dots.set(2, 0, 0.0, false);
        dots.set(3, 2, 10.0, false);
        let looks = dots.looks(10.0);
        assert_eq!(looks.len(), 3);
        assert_eq!(looks[2].slot, 26.0);
        assert!(!dots.prune(10.0));
    }
}
