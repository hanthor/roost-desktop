//! GNOME's Multitasking and window-manager preferences (#337, #347):
//! the pure policy half. The window manager owns the state; this module
//! decides what a modifier-button press does, when pointer focus moves
//! (focus-follows-mouse with Mutter's pointer rest and auto-raise),
//! where a window goes on another monitor or a work-area spot, and how
//! an axis maximize toggles. Everything is geometry or small state
//! machines, so it is unit-tested without a seat or a renderer.
//!
//! Behaviour follows Mutter 51 (core/display.c focus modes,
//! core/window.c button handling, core/keybindings.c handlers).

use smithay::utils::{Logical, Point, Rectangle};
use tuna_shell_control::{Direction, FocusMode, Gravity, WmSettings};

/// evdev buttons (linux/input-event-codes.h).
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

/// Mutter's pointer-rest check: focus moves once the pointer has been
/// still this long (`FOCUS_TIMEOUT_DELAY`).
pub const POINTER_REST_MS: u64 = 25;

/// What a press with `mouse-button-modifier` held does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRole {
    /// Move the window with the pointer.
    Move,
    /// Resize it from the edges nearest the pointer.
    Resize,
    /// Open the window menu at the pointer.
    Menu,
}

/// The role of `button` pressed with `held` modifiers (`MOD_*` bits):
/// none unless every bit of `mouse-button-modifier` is held (Mutter's
/// `is_window_grab`). The left button moves; the resize button is the
/// middle one, or the right one with `resize-with-right-button`, and
/// the other of the two opens the menu.
pub fn button_role(settings: &WmSettings, held: u32, button: u32) -> Option<ButtonRole> {
    let modifier = settings.mouse_button_modifier;
    if modifier == 0 || held & modifier != modifier {
        return None;
    }
    let (resize, menu) = if settings.resize_with_right_button {
        (BTN_RIGHT, BTN_MIDDLE)
    } else {
        (BTN_MIDDLE, BTN_RIGHT)
    };
    match button {
        BTN_LEFT => Some(ButtonRole::Move),
        b if b == resize => Some(ButtonRole::Resize),
        b if b == menu => Some(ButtonRole::Menu),
        _ => None,
    }
}

/// xdg_toplevel resize edges for a modifier resize grabbed at `pos`:
/// the window's outer thirds pick the edges (Mutter); the middle ninth
/// picks none, and no resize starts.
pub fn resize_edges(geometry: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> u32 {
    const TOP: u32 = 1;
    const BOTTOM: u32 = 2;
    const LEFT: u32 = 4;
    const RIGHT: u32 = 8;
    let (x, y) = (f64::from(geometry.loc.x), f64::from(geometry.loc.y));
    let (w, h) = (f64::from(geometry.size.w), f64::from(geometry.size.h));
    let mut edges = 0;
    if pos.x < x + w / 3.0 {
        edges |= LEFT;
    }
    if pos.x > x + 2.0 * w / 3.0 {
        edges |= RIGHT;
    }
    if pos.y < y + h / 3.0 {
        edges |= TOP;
    }
    if pos.y > y + 2.0 * h / 3.0 {
        edges |= BOTTOM;
    }
    edges
}

/// What focus-follows-mouse asks the window manager to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverEvent {
    /// Focus this window without raising it.
    Focus(u64),
    /// Drop keyboard focus (`mouse` mode over the bare desktop).
    Unfocus,
    /// Raise this window (`auto-raise` after its delay).
    Raise(u64),
}

/// A focus change waiting for the pointer to rest.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pending {
    target: Option<u64>,
    pos: (i32, i32),
    since_ms: u64,
}

/// GNOME's sloppy and mouse focus modes. Focus follows the pointer
/// only when it crosses into another window by moving: a window that
/// slides under a still pointer (a restack, an Alt+Tab, an unmap) never
/// takes focus, which is Mutter's sticky focus. With
/// `focus-change-on-pointer-rest` the change waits until the pointer
/// stops; `auto-raise` raises the focused window after its delay if the
/// pointer is still over it.
#[derive(Debug, Default)]
pub struct HoverFocus {
    /// The pointer target at the last motion (`Some(None)`: the bare
    /// desktop), and where the pointer was.
    last: Option<(Option<u64>, (i32, i32))>,
    pending: Option<Pending>,
    raise: Option<(u64, u64)>,
}

impl HoverFocus {
    /// Forget everything (the mode changed, a grab or the overview took
    /// the pointer).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The pointer moved to `pos` over `target` (`None`: no window).
    /// Returns what to do now; with pointer rest the change waits for
    /// [`tick`](Self::tick).
    pub fn motion(
        &mut self,
        settings: &WmSettings,
        target: Option<u64>,
        pos: Point<f64, Logical>,
        now_ms: u64,
    ) -> Option<HoverEvent> {
        if settings.focus_mode == FocusMode::Click {
            self.reset();
            return None;
        }
        let pos = (pos.x.floor() as i32, pos.y.floor() as i32);
        let previous = self.last.replace((target, pos));
        if let Some(pending) = self.pending.as_mut() {
            if pending.pos != pos {
                pending.pos = pos;
                pending.since_ms = now_ms;
            }
        }
        if let Some((raising, _)) = self.raise {
            if target != Some(raising) {
                self.raise = None;
            }
        }
        let crossed = match previous {
            // The first motion seen: nothing was entered yet.
            None => false,
            Some((before, before_pos)) => before != target && before_pos != pos,
        };
        if !crossed {
            return None;
        }
        let pending = Pending {
            target,
            pos,
            since_ms: now_ms,
        };
        if settings.focus_change_on_pointer_rest {
            self.pending = Some(pending);
            None
        } else {
            self.pending = None;
            self.decide(settings, target, now_ms)
        }
    }

    /// Advance time: a rested pending change lands, a due auto-raise
    /// fires. `target` is what is under the pointer now.
    pub fn tick(
        &mut self,
        settings: &WmSettings,
        target: Option<u64>,
        now_ms: u64,
    ) -> Vec<HoverEvent> {
        let mut out = Vec::new();
        if settings.focus_mode == FocusMode::Click {
            self.reset();
            return out;
        }
        if let Some(pending) = self.pending {
            if now_ms.saturating_sub(pending.since_ms) >= POINTER_REST_MS {
                self.pending = None;
                // Mutter drops the change when the pointer has left the
                // window it rested over.
                if pending.target == target {
                    out.extend(self.decide(settings, target, now_ms));
                }
            }
        }
        if let Some((id, due)) = self.raise {
            if target != Some(id) {
                self.raise = None;
            } else if now_ms >= due {
                self.raise = None;
                out.push(HoverEvent::Raise(id));
            }
        }
        out
    }

    fn decide(
        &mut self,
        settings: &WmSettings,
        target: Option<u64>,
        now_ms: u64,
    ) -> Option<HoverEvent> {
        match target {
            Some(id) => {
                if settings.auto_raise {
                    let delay = u64::from(settings.auto_raise_delay_ms);
                    self.raise = Some((id, now_ms.saturating_add(delay)));
                }
                Some(HoverEvent::Focus(id))
            }
            None if settings.focus_mode == FocusMode::Mouse => Some(HoverEvent::Unfocus),
            None => None,
        }
    }
}

/// The output next to `from` in `direction` among `outputs`: the
/// nearest one whose span overlaps `from` across the direction and lies
/// wholly beyond its edge (Mutter's `meta_display_get_monitor_neighbor`).
pub fn neighbor_output(
    outputs: &[Rectangle<i32, Logical>],
    from: Rectangle<i32, Logical>,
    direction: Direction,
) -> Option<Rectangle<i32, Logical>> {
    let overlaps_y = |o: &Rectangle<i32, Logical>| {
        o.loc.y < from.loc.y + from.size.h && from.loc.y < o.loc.y + o.size.h
    };
    let overlaps_x = |o: &Rectangle<i32, Logical>| {
        o.loc.x < from.loc.x + from.size.w && from.loc.x < o.loc.x + o.size.w
    };
    outputs
        .iter()
        .copied()
        .filter(|o| *o != from)
        .filter(|o| match direction {
            Direction::Left => overlaps_y(o) && o.loc.x + o.size.w <= from.loc.x,
            Direction::Right => overlaps_y(o) && o.loc.x >= from.loc.x + from.size.w,
            Direction::Up => overlaps_x(o) && o.loc.y + o.size.h <= from.loc.y,
            Direction::Down => overlaps_x(o) && o.loc.y >= from.loc.y + from.size.h,
        })
        .min_by_key(|o| match direction {
            Direction::Left => from.loc.x - (o.loc.x + o.size.w),
            Direction::Right => o.loc.x - (from.loc.x + from.size.w),
            Direction::Up => from.loc.y - (o.loc.y + o.size.h),
            Direction::Down => o.loc.y - (from.loc.y + from.size.h),
        })
}

/// `geometry` carried from output `from` to output `to`, keeping its
/// place relative to the output's size and staying inside `to`.
pub fn carry_to_output(
    geometry: Rectangle<i32, Logical>,
    from: Rectangle<i32, Logical>,
    to: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let scale = |offset: i32, from_len: i32, to_len: i32| {
        if from_len <= 0 {
            0
        } else {
            (f64::from(offset) * f64::from(to_len) / f64::from(from_len)).round() as i32
        }
    };
    let w = geometry.size.w.min(to.size.w);
    let h = geometry.size.h.min(to.size.h);
    let x = to.loc.x + scale(geometry.loc.x - from.loc.x, from.size.w, to.size.w);
    let y = to.loc.y + scale(geometry.loc.y - from.loc.y, from.size.h, to.size.h);
    let x = x.clamp(to.loc.x, to.loc.x + to.size.w - w);
    let y = y.clamp(to.loc.y, to.loc.y + to.size.h - h);
    Rectangle::new((x, y).into(), (w, h).into())
}

/// Where `geometry` goes for `gravity` in `work` (Mutter's
/// `move-to-corner-*`, `move-to-side-*` and `move-to-center`): sides
/// keep the other axis, corners set both, the center centers.
pub fn gravity_position(
    work: Rectangle<i32, Logical>,
    geometry: Rectangle<i32, Logical>,
    gravity: Gravity,
) -> Point<i32, Logical> {
    let left = work.loc.x;
    let right = work.loc.x + work.size.w - geometry.size.w;
    let top = work.loc.y;
    let bottom = work.loc.y + work.size.h - geometry.size.h;
    let center_x = work.loc.x + (work.size.w - geometry.size.w) / 2;
    let center_y = work.loc.y + (work.size.h - geometry.size.h) / 2;
    let (x, y) = match gravity {
        Gravity::NorthWest => (left, top),
        Gravity::North => (geometry.loc.x, top),
        Gravity::NorthEast => (right, top),
        Gravity::West => (left, geometry.loc.y),
        Gravity::Center => (center_x, center_y),
        Gravity::East => (right, geometry.loc.y),
        Gravity::SouthWest => (left, bottom),
        Gravity::South => (geometry.loc.x, bottom),
        Gravity::SouthEast => (right, bottom),
    };
    (x, y).into()
}

/// Mutter's axis maximize toggle: fill `work` along one axis (keeping
/// the other), or give back `restore` when already filling it.
/// Returns the new geometry and what to restore later (`None` when the
/// toggle gave the old geometry back).
pub fn toggle_axis_maximize(
    work: Rectangle<i32, Logical>,
    geometry: Rectangle<i32, Logical>,
    restore: Option<Rectangle<i32, Logical>>,
    vertical: bool,
) -> (Rectangle<i32, Logical>, Option<Rectangle<i32, Logical>>) {
    let filled = if vertical {
        geometry.loc.y == work.loc.y && geometry.size.h == work.size.h
    } else {
        geometry.loc.x == work.loc.x && geometry.size.w == work.size.w
    };
    if filled {
        if let Some(before) = restore {
            let mut back = geometry;
            if vertical {
                back.loc.y = before.loc.y;
                back.size.h = before.size.h;
            } else {
                back.loc.x = before.loc.x;
                back.size.w = before.size.w;
            }
            return (back, None);
        }
        return (geometry, None);
    }
    let mut next = geometry;
    if vertical {
        next.loc.y = work.loc.y;
        next.size.h = work.size.h;
    } else {
        next.loc.x = work.loc.x;
        next.size.w = work.size.w;
    }
    (next, Some(geometry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::{FocusMode, MOD_ALT, MOD_LOGO, MOD_SHIFT};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn at(x: f64, y: f64) -> Point<f64, Logical> {
        (x, y).into()
    }

    fn sloppy() -> WmSettings {
        WmSettings {
            focus_mode: FocusMode::Sloppy,
            focus_change_on_pointer_rest: false,
            ..WmSettings::default()
        }
    }

    #[test]
    fn modifier_buttons_follow_gnomes_defaults_and_preferences() {
        let gnome = WmSettings::default();
        assert_eq!(
            button_role(&gnome, MOD_LOGO, BTN_LEFT),
            Some(ButtonRole::Move)
        );
        assert_eq!(
            button_role(&gnome, MOD_LOGO, BTN_MIDDLE),
            Some(ButtonRole::Resize)
        );
        assert_eq!(
            button_role(&gnome, MOD_LOGO, BTN_RIGHT),
            Some(ButtonRole::Menu)
        );
        // Extra modifiers still count (Mutter's mask test); missing does not.
        assert_eq!(
            button_role(&gnome, MOD_LOGO | MOD_SHIFT, BTN_LEFT),
            Some(ButtonRole::Move)
        );
        assert_eq!(button_role(&gnome, MOD_ALT, BTN_LEFT), None);
        assert_eq!(button_role(&gnome, 0, BTN_LEFT), None);
        let alt_right = WmSettings {
            mouse_button_modifier: MOD_ALT,
            resize_with_right_button: true,
            ..gnome
        };
        assert_eq!(
            button_role(&alt_right, MOD_ALT, BTN_RIGHT),
            Some(ButtonRole::Resize)
        );
        assert_eq!(
            button_role(&alt_right, MOD_ALT, BTN_MIDDLE),
            Some(ButtonRole::Menu)
        );
        assert_eq!(button_role(&alt_right, MOD_LOGO, BTN_LEFT), None);
        let disabled = WmSettings {
            mouse_button_modifier: 0,
            ..gnome
        };
        assert_eq!(button_role(&disabled, MOD_LOGO, BTN_LEFT), None);
    }

    #[test]
    fn modifier_resize_picks_the_nearest_edges_by_thirds() {
        let window = rect(0, 0, 300, 300);
        assert_eq!(resize_edges(window, at(10.0, 10.0)), 1 | 4);
        assert_eq!(resize_edges(window, at(290.0, 290.0)), 2 | 8);
        assert_eq!(resize_edges(window, at(150.0, 290.0)), 2);
        assert_eq!(resize_edges(window, at(150.0, 150.0)), 0, "middle ninth");
    }

    #[test]
    fn click_focus_never_follows_the_pointer() {
        let mut hover = HoverFocus::default();
        let click = WmSettings::default();
        assert_eq!(hover.motion(&click, Some(1), at(0.0, 0.0), 0), None);
        assert_eq!(hover.motion(&click, Some(2), at(5.0, 0.0), 1), None);
        assert!(hover.tick(&click, Some(2), 1000).is_empty());
    }

    #[test]
    fn sloppy_focus_follows_crossings_and_keeps_focus_over_the_desktop() {
        let mut hover = HoverFocus::default();
        let wm = sloppy();
        assert_eq!(hover.motion(&wm, Some(1), at(0.0, 0.0), 0), None);
        assert_eq!(
            hover.motion(&wm, Some(2), at(10.0, 0.0), 1),
            Some(HoverEvent::Focus(2))
        );
        // Moving within the window is not a crossing.
        assert_eq!(hover.motion(&wm, Some(2), at(20.0, 0.0), 2), None);
        // The bare desktop keeps the focus in sloppy mode...
        assert_eq!(hover.motion(&wm, None, at(30.0, 0.0), 3), None);
        // ...and takes it in mouse mode.
        let mouse = WmSettings {
            focus_mode: FocusMode::Mouse,
            ..wm
        };
        assert_eq!(
            hover.motion(&mouse, Some(2), at(40.0, 0.0), 4),
            Some(HoverEvent::Focus(2))
        );
        assert_eq!(
            hover.motion(&mouse, None, at(50.0, 0.0), 5),
            Some(HoverEvent::Unfocus)
        );
    }

    #[test]
    fn a_window_sliding_under_a_still_pointer_never_steals_focus() {
        let mut hover = HoverFocus::default();
        let wm = sloppy();
        hover.motion(&wm, Some(1), at(10.0, 10.0), 0);
        // Alt+Tab raised window 2 under the pointer; nothing moved.
        assert_eq!(hover.motion(&wm, Some(2), at(10.0, 10.0), 5), None);
        // A real move inside it is not a crossing either.
        assert_eq!(hover.motion(&wm, Some(2), at(12.0, 10.0), 6), None);
    }

    #[test]
    fn pointer_rest_waits_for_the_pointer_to_stop() {
        let mut hover = HoverFocus::default();
        let wm = WmSettings {
            focus_change_on_pointer_rest: true,
            ..sloppy()
        };
        hover.motion(&wm, Some(1), at(0.0, 0.0), 0);
        assert_eq!(hover.motion(&wm, Some(2), at(10.0, 0.0), 100), None);
        assert!(hover.tick(&wm, Some(2), 110).is_empty(), "still moving");
        // It keeps moving: the rest restarts.
        hover.motion(&wm, Some(2), at(12.0, 0.0), 120);
        assert!(hover.tick(&wm, Some(2), 140).is_empty());
        assert_eq!(hover.tick(&wm, Some(2), 145), vec![HoverEvent::Focus(2)]);
        // Passing through a window on the way elsewhere focuses nothing.
        hover.motion(&wm, Some(3), at(20.0, 0.0), 200);
        hover.motion(&wm, Some(4), at(30.0, 0.0), 210);
        assert!(hover.tick(&wm, Some(4), 220).is_empty());
        assert_eq!(hover.tick(&wm, Some(4), 240), vec![HoverEvent::Focus(4)]);
    }

    #[test]
    fn auto_raise_fires_after_its_delay_while_the_pointer_stays() {
        let mut hover = HoverFocus::default();
        let wm = WmSettings {
            auto_raise: true,
            auto_raise_delay_ms: 500,
            ..sloppy()
        };
        hover.motion(&wm, Some(1), at(0.0, 0.0), 0);
        assert_eq!(
            hover.motion(&wm, Some(2), at(10.0, 0.0), 1000),
            Some(HoverEvent::Focus(2))
        );
        assert!(hover.tick(&wm, Some(2), 1400).is_empty());
        assert_eq!(hover.tick(&wm, Some(2), 1500), vec![HoverEvent::Raise(2)]);
        assert!(hover.tick(&wm, Some(2), 2000).is_empty(), "once");
        // Leaving before the delay cancels it.
        hover.motion(&wm, Some(3), at(20.0, 0.0), 3000);
        hover.motion(&wm, Some(1), at(30.0, 0.0), 3100);
        assert_eq!(hover.tick(&wm, Some(1), 3550), Vec::new());
    }

    #[test]
    fn monitor_neighbors_follow_the_layout() {
        let a = rect(0, 0, 1280, 800);
        let b = rect(1280, 0, 1920, 1080);
        let c = rect(0, 800, 1280, 800);
        let outputs = [a, b, c];
        assert_eq!(neighbor_output(&outputs, a, Direction::Right), Some(b));
        assert_eq!(neighbor_output(&outputs, b, Direction::Left), Some(a));
        assert_eq!(neighbor_output(&outputs, a, Direction::Down), Some(c));
        assert_eq!(neighbor_output(&outputs, a, Direction::Left), None);
        assert_eq!(neighbor_output(&outputs, c, Direction::Up), Some(a));
    }

    #[test]
    fn windows_keep_their_relative_place_on_another_monitor() {
        let a = rect(0, 0, 1000, 800);
        let b = rect(1000, 0, 2000, 1600);
        let moved = carry_to_output(rect(100, 80, 400, 300), a, b);
        assert_eq!(moved, rect(1200, 160, 400, 300));
        // Too big for the target: shrunk and kept inside.
        let small = rect(1000, 0, 300, 200);
        assert_eq!(
            carry_to_output(rect(700, 600, 400, 300), a, small),
            rect(1000, 0, 300, 200)
        );
    }

    #[test]
    fn gravities_place_like_mutters_move_keys() {
        let work = rect(0, 32, 1000, 768);
        let window = rect(100, 100, 200, 100);
        assert_eq!(
            gravity_position(work, window, Gravity::NorthWest),
            (0, 32).into()
        );
        assert_eq!(
            gravity_position(work, window, Gravity::SouthEast),
            (800, 700).into()
        );
        assert_eq!(
            gravity_position(work, window, Gravity::North),
            (100, 32).into()
        );
        assert_eq!(
            gravity_position(work, window, Gravity::East),
            (800, 100).into()
        );
        assert_eq!(
            gravity_position(work, window, Gravity::Center),
            (400, 366).into()
        );
    }

    #[test]
    fn axis_maximize_toggles_one_axis_and_back() {
        let work = rect(0, 32, 1000, 768);
        let window = rect(100, 100, 200, 100);
        let (tall, restore) = toggle_axis_maximize(work, window, None, true);
        assert_eq!(tall, rect(100, 32, 200, 768));
        let (back, cleared) = toggle_axis_maximize(work, tall, restore, true);
        assert_eq!(back, window);
        assert_eq!(cleared, None);
        let (wide, _) = toggle_axis_maximize(work, window, None, false);
        assert_eq!(wide, rect(0, 100, 1000, 100));
    }
}
