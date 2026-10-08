//! GNOME 51's transient-surface animations (#501): the OSD, notification
//! banners and list items, modal dialogs with their lightbox, the panel
//! menus and the quick-settings submenus.
//!
//! Every animation is split into a fade part (opacity) and a motion part
//! (slide, rise, scale, height). GNOME keeps the fade under Reduced
//! Motion and drops the motion; [`motion_allowed`] is the one place that
//! decides it. With animations off everything lands on its final frame at
//! once.
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// `osdWindow.js` FADE_TIME and LEVEL_ANIMATION_TIME.
pub const OSD_MS: f64 = 100.0;
/// `messageTray.js` ANIMATION_TIME: banners in and out.
pub const BANNER_MS: f64 = 200.0;
/// A banner grows from 90% as it drops in.
const BANNER_SCALE: f64 = 0.9;
/// `messageList.js` MESSAGE_ANIMATION_TIME: list items added or removed.
pub const MESSAGE_MS: f64 = 100.0;
/// `modalDialog.js` OPEN_AND_CLOSE_TIME, also the lightbox fade.
pub const MODAL_MS: f64 = 100.0;
/// `boxpointer.js` POPUP_ANIMATION_TIME: panel menus.
pub const MENU_MS: f64 = 150.0;
/// `quickSettings.js` POPUP_ANIMATION_TIME: a submenu opens in two
/// halves, height then fade (closing: fade then height).
pub const SUBMENU_MS: f64 = 400.0;

/// Whether GTK animations are on (the shell mirrors GNOME's
/// enable-animations and Reduced Motion into this setting).
pub fn animations() -> bool {
    gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations())
}

/// Whether the motion part plays beside the fade. GNOME 51 keeps fades
/// under Reduced Motion; the shell's shared motion policy (#493) is
/// where that distinction will come from. Until then Reduced Motion
/// turns all animation off, so motion simply follows it.
pub fn motion_allowed() -> bool {
    animations()
}

/// Clutter's EASE_OUT_QUAD.
pub fn ease_out_quad(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Clutter's EASE_OUT_BACK: overshoots by about 10% before settling.
pub fn ease_out_back(t: f64) -> f64 {
    const S: f64 = 1.70158;
    if t <= 0.0 {
        return 0.0;
    }
    let p = t.min(1.0) - 1.0;
    p * p * ((S + 1.0) * p + S) + 1.0
}

/// Linear progress through a `duration_ms` animation; complete at once
/// when animations are off.
pub fn progress(elapsed_ms: f64, duration_ms: f64, enabled: bool) -> f64 {
    if !enabled || duration_ms <= 0.0 {
        1.0
    } else {
        (elapsed_ms / duration_ms).clamp(0.0, 1.0)
    }
}

pub fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

/// One frame of a transient surface: the fade part (`opacity`) and the
/// motion part (vertical offset, scale about the pivot, and the share of
/// its height it takes up in layout).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub opacity: f64,
    pub dy: f64,
    pub scale: f64,
    pub extent: f64,
}

impl Frame {
    pub const REST: Frame = Frame {
        opacity: 1.0,
        dy: 0.0,
        scale: 1.0,
        extent: 1.0,
    };

    /// Only the fade part, as GNOME plays it under Reduced Motion.
    pub fn without_motion(self) -> Frame {
        Frame {
            opacity: self.opacity,
            ..Frame::REST
        }
    }

    fn gated(self, motion: bool) -> Frame {
        if motion {
            self
        } else {
            self.without_motion()
        }
    }
}

/// A banner dropping in (`_showNotification`): it fades in (ease-out
/// quad) while sliding down from its own height above and growing from
/// 90% (ease-out back).
pub fn banner_enter(t: f64, height: f64) -> Frame {
    let back = ease_out_back(t);
    Frame {
        opacity: ease_out_quad(t),
        dy: -height * (1.0 - back),
        scale: lerp(BANNER_SCALE, 1.0, back),
        extent: 1.0,
    }
}

/// A banner leaving (`_hideNotification`): fade and slide back up, both
/// ease-out back.
pub fn banner_leave(t: f64, height: f64) -> Frame {
    let back = ease_out_back(t);
    Frame {
        opacity: (1.0 - back).clamp(0.0, 1.0),
        dy: -height * back,
        scale: 1.0,
        extent: 1.0,
    }
}

/// A list item zooming in (`_addMessage`) or out (`_removeMessage`):
/// GNOME scales the item, its layout share with it.
pub fn message(t: f64, adding: bool) -> Frame {
    let e = ease_out_quad(t);
    let shown = if adding { e } else { 1.0 - e };
    Frame {
        opacity: shown,
        dy: 0.0,
        scale: shown,
        extent: shown,
    }
}

/// A quick-settings submenu at `t` of [`SUBMENU_MS`]. Opening grows the
/// height over the first half, then fades the content in; closing
/// fades it out, then shrinks.
pub fn submenu(t: f64, opening: bool) -> Frame {
    let first = ease_out_quad(t * 2.0);
    let second = ease_out_quad(t * 2.0 - 1.0);
    let (extent, opacity) = if opening {
        (first, second)
    } else {
        (1.0 - second, 1.0 - first)
    };
    Frame {
        opacity,
        dy: 0.0,
        scale: 1.0,
        extent,
    }
}

/// A plain ease-out-quad fade from `from` to `to` (OSD, modals, menus).
pub fn fade(from: f64, to: f64, t: f64) -> f64 {
    lerp(from, to, ease_out_quad(t))
}

/// Drives one animation on a widget's frame clock. Starting another on
/// the same player supersedes the running one, whose `done` never runs.
///
/// With `TUNA_TRANSIENT_TRACE` set, each animation logs whether it ran
/// instantly or when it settled (the GTK shell proof's G-TRANSIENT gates).
#[derive(Clone)]
pub struct Player {
    generation: Rc<Cell<u64>>,
    name: &'static str,
}

fn trace(name: &str, what: std::fmt::Arguments) {
    if std::env::var_os("TUNA_TRANSIENT_TRACE").is_some() {
        eprintln!("tuna-shell-gtk: transient {name} {what}");
    }
}

impl Player {
    pub fn new(name: &'static str) -> Self {
        Self {
            generation: Rc::default(),
            name,
        }
    }

    /// Cancel the running animation, if any.
    pub fn stop(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Call `step` with linear progress 0..=1 each frame for
    /// `duration_ms`, then `done`. With animations off, or on a hidden
    /// widget (which gets no frames), `step(1.0)` and `done` run now.
    pub fn play(
        &self,
        widget: &impl IsA<gtk::Widget>,
        duration_ms: f64,
        step: impl Fn(f64) + 'static,
        done: impl FnOnce() + 'static,
    ) {
        self.stop();
        let generation = self.generation.get();
        let name = self.name;
        if !animations() || !widget.is_visible() {
            trace(name, format_args!("instant"));
            step(1.0);
            done();
            return;
        }
        step(0.0);
        let current = self.generation.clone();
        let started = Cell::new(None);
        let done = RefCell::new(Some(done));
        widget.add_tick_callback(move |_, clock| {
            if current.get() != generation {
                return glib::ControlFlow::Break;
            }
            let start = started.get().unwrap_or_else(|| clock.frame_time());
            started.set(Some(start));
            let elapsed_ms = (clock.frame_time() - start) as f64 / 1000.0;
            let t = progress(elapsed_ms, duration_ms, animations());
            step(t);
            if t < 1.0 {
                return glib::ControlFlow::Continue;
            }
            trace(
                name,
                format_args!("settled after {elapsed_ms:.0} ms of {duration_ms:.0} ms"),
            );
            if let Some(done) = done.borrow_mut().take() {
                done();
            }
            glib::ControlFlow::Break
        });
    }
}

mod imp {
    use super::*;

    pub struct MotionBin {
        pub dy: Cell<f64>,
        pub scale: Cell<f64>,
        pub extent: Cell<f64>,
    }

    impl Default for MotionBin {
        fn default() -> Self {
            Self {
                dy: Cell::new(0.0),
                scale: Cell::new(1.0),
                extent: Cell::new(1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MotionBin {
        const NAME: &'static str = "TunaMotionBin";
        type Type = super::MotionBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MotionBin {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MotionBin {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            let (min, natural, _, _) = child.measure(orientation, for_size);
            if orientation == gtk::Orientation::Vertical {
                let extent = self.extent.get();
                let share = |v: i32| (f64::from(v) * extent).round() as i32;
                (share(min), share(natural), -1, -1)
            } else {
                (min, natural, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            // The child keeps its full height while the bin's layout
            // share shrinks; the bin clips the rest.
            let (_, natural, _, _) = child.measure(gtk::Orientation::Vertical, width);
            let child_height = height.max(natural);
            // Scale about the centre, as GNOME's pivot (0.5, 0.5).
            let (cx, cy) = (f64::from(width) / 2.0, f64::from(child_height) / 2.0);
            let scale = self.scale.get().max(0.0) as f32;
            let transform = gtk::gsk::Transform::new()
                .translate(&gtk::graphene::Point::new(
                    cx as f32,
                    (cy + self.dy.get()) as f32,
                ))
                .scale(scale, scale)
                .translate(&gtk::graphene::Point::new(-cx as f32, -cy as f32));
            child.allocate(width, child_height, -1, Some(transform));
        }
    }
}

glib::wrapper! {
    /// A one-child container that shows its child at a [`Frame`].
    pub struct MotionBin(ObjectSubclass<imp::MotionBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MotionBin {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let bin: Self = glib::Object::new();
        child.set_parent(&bin);
        bin
    }

    pub fn set_frame(&self, frame: Frame) {
        let imp = self.imp();
        self.set_opacity(frame.opacity.clamp(0.0, 1.0));
        imp.dy.set(frame.dy);
        imp.scale.set(frame.scale);
        let resized = imp.extent.replace(frame.extent) != frame.extent;
        // Clip only while something is moving.
        self.set_overflow(if frame == Frame::REST {
            gtk::Overflow::Visible
        } else {
            gtk::Overflow::Hidden
        });
        if resized {
            self.queue_resize();
        } else {
            self.queue_allocate();
        }
    }

    /// Play `frame_at` (given the bin and linear progress) over
    /// `duration_ms`, motion gated by [`motion_allowed`].
    pub fn animate(
        &self,
        player: &Player,
        duration_ms: f64,
        frame_at: impl Fn(&MotionBin, f64) -> Frame + 'static,
        done: impl FnOnce() + 'static,
    ) {
        let bin = self.downgrade();
        let motion = motion_allowed();
        player.play(
            self,
            duration_ms,
            move |t| {
                if let Some(bin) = bin.upgrade() {
                    bin.set_frame(frame_at(&bin, t).gated(motion));
                }
            },
            done,
        );
    }
}

/// A modal window (polkit, end-session, network secrets) that fades in
/// and out with its lightbox, as `ModalDialog` and `Lightbox` do. The
/// window's own background is the lightbox, so one opacity covers both.
pub struct ModalFade {
    window: gtk::Window,
    player: Player,
}

impl ModalFade {
    pub fn new(window: &impl IsA<gtk::Window>) -> Self {
        Self {
            window: window.clone().upcast(),
            player: Player::new("modal"),
        }
    }

    /// Present the window, fading in from wherever it stands.
    pub fn present(&self) {
        let from = if self.window.is_visible() {
            self.window.opacity()
        } else {
            0.0
        };
        self.window.set_can_target(true);
        self.window.set_opacity(from);
        self.window.present();
        let window = self.window.downgrade();
        self.player.play(
            &self.window,
            MODAL_MS,
            move |t| {
                if let Some(w) = window.upgrade() {
                    w.set_opacity(fade(from, 1.0, t));
                }
            },
            || {},
        );
    }

    /// Fade out, then hide. The dialog takes no more clicks meanwhile.
    pub fn hide(&self) {
        if !self.window.is_visible() {
            return;
        }
        self.window.set_can_target(false);
        let from = self.window.opacity();
        let (step, done) = (self.window.downgrade(), self.window.downgrade());
        self.player.play(
            &self.window,
            MODAL_MS,
            move |t| {
                if let Some(w) = step.upgrade() {
                    w.set_opacity(fade(from, 0.0, t));
                }
            },
            move || {
                if let Some(w) = done.upgrade() {
                    w.set_visible(false);
                    w.set_opacity(1.0);
                    w.set_can_target(true);
                }
            },
        );
    }
}

/// Fade a panel menu in as it opens (`BoxPointer.open`). GTK closes a
/// popover at once, so only the opening plays here; the rise toward the
/// anchor needs the popup surface itself to move, which is left to the
/// compositor's popup animation.
pub fn fade_in_popover(popover: &gtk::Popover) {
    let player = Player::new("menu");
    popover.connect_map(move |popover| {
        popover.set_opacity(0.0);
        let weak = popover.downgrade();
        player.play(
            popover,
            MENU_MS,
            move |t| {
                if let Some(p) = weak.upgrade() {
                    p.set_opacity(fade(0.0, 1.0, t));
                }
            },
            || {},
        );
    });
}

/// A quick-settings submenu that grows open and shrinks closed. `outer`
/// is the menu's visibility (what the rest of the panel follows);
/// `bin` wraps its content.
pub struct Submenu {
    outer: gtk::Box,
    bin: MotionBin,
    player: Player,
    closing: Cell<bool>,
}

impl Submenu {
    pub fn new(outer: &gtk::Box, bin: &MotionBin) -> Rc<Self> {
        let menu = Rc::new(Self {
            outer: outer.clone(),
            bin: bin.clone(),
            player: Player::new("submenu"),
            closing: Cell::new(false),
        });
        let weak = Rc::downgrade(&menu);
        outer.connect_visible_notify(move |outer| {
            let Some(menu) = weak.upgrade() else {
                return;
            };
            menu.closing.set(false);
            if outer.is_visible() {
                menu.play_open();
            } else {
                // Closed outright (the panel went away): settle.
                menu.player.stop();
                menu.bin.set_frame(Frame::REST);
            }
        });
        menu
    }

    fn play_open(&self) {
        self.bin.set_frame(submenu(0.0, true));
        self.bin
            .animate(&self.player, SUBMENU_MS, |_, t| submenu(t, true), || {});
    }

    /// Open, and not on its way closed.
    pub fn is_open(&self) -> bool {
        self.outer.is_visible() && !self.closing.get()
    }

    pub fn set_open(&self, open: bool) {
        if open {
            if self.outer.is_visible() {
                // Reopened while closing: settle open.
                self.closing.set(false);
                self.player.stop();
                self.bin.set_frame(Frame::REST);
            } else {
                self.outer.set_visible(true);
            }
            return;
        }
        if !self.is_open() {
            return;
        }
        self.closing.set(true);
        let outer = self.outer.downgrade();
        self.bin.animate(
            &self.player,
            SUBMENU_MS,
            |_, t| submenu(t, false),
            move || {
                if let Some(outer) = outer.upgrade() {
                    outer.set_visible(false);
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn easing_curves_match_clutter() {
        assert_eq!(ease_out_quad(0.0), 0.0);
        assert_eq!(ease_out_quad(0.5), 0.75);
        assert_eq!(ease_out_quad(1.0), 1.0);
        assert_eq!(ease_out_quad(2.0), 1.0);
        assert!(close(ease_out_back(0.0), 0.0));
        assert!(close(ease_out_back(1.0), 1.0));
        // Ease-out back overshoots by about 10% before settling.
        let peak = (0..=100)
            .map(|i| ease_out_back(f64::from(i) / 100.0))
            .fold(0.0, f64::max);
        assert!(peak > 1.09 && peak < 1.11, "{peak}");
    }

    #[test]
    fn progress_is_linear_and_instant_when_animations_are_off() {
        assert_eq!(progress(0.0, 100.0, true), 0.0);
        assert_eq!(progress(50.0, 100.0, true), 0.5);
        assert_eq!(progress(150.0, 100.0, true), 1.0);
        assert_eq!(progress(0.0, 100.0, false), 1.0);
        assert_eq!(progress(0.0, 0.0, true), 1.0);
    }

    #[test]
    fn gnome_51_durations() {
        assert_eq!(OSD_MS, 100.0);
        assert_eq!(BANNER_MS, 200.0);
        assert_eq!(MESSAGE_MS, 100.0);
        assert_eq!(MODAL_MS, 100.0);
        assert_eq!(MENU_MS, 150.0);
        // Two 200 ms halves.
        assert_eq!(SUBMENU_MS, 400.0);
    }

    #[test]
    fn banners_drop_in_scaled_and_slide_back_up() {
        let start = banner_enter(0.0, 80.0);
        assert_eq!(start.opacity, 0.0);
        assert!(close(start.dy, -80.0));
        assert!(close(start.scale, 0.9));
        let end = banner_enter(1.0, 80.0);
        assert!(close(end.opacity, 1.0) && close(end.dy, 0.0) && close(end.scale, 1.0));
        // Ease-out back overshoots the rest position mid-way.
        assert!(banner_enter(0.7, 80.0).scale > 1.0);
        assert!(banner_enter(0.7, 80.0).dy > 0.0);
        let gone = banner_leave(1.0, 80.0);
        assert!(close(gone.opacity, 0.0) && close(gone.dy, -80.0));
        assert_eq!(banner_leave(0.0, 80.0).opacity, 1.0);
        // Opacity never leaves 0..=1 despite the overshoot.
        for i in 0..=20 {
            let t = f64::from(i) / 20.0;
            assert!((0.0..=1.0).contains(&banner_leave(t, 80.0).opacity));
        }
    }

    #[test]
    fn list_items_zoom_in_and_out() {
        assert_eq!(message(0.0, true).scale, 0.0);
        assert_eq!(message(0.5, true).extent, 0.75);
        assert_eq!(message(1.0, true), Frame::REST);
        assert_eq!(message(0.0, false), Frame::REST);
        assert_eq!(message(0.5, false).scale, 0.25);
        assert_eq!(message(1.0, false).extent, 0.0);
    }

    #[test]
    fn submenus_grow_then_fade_and_fade_then_shrink() {
        assert_eq!(submenu(0.0, true).extent, 0.0);
        assert_eq!(submenu(0.25, true).extent, 0.75);
        assert_eq!(submenu(0.25, true).opacity, 0.0);
        assert_eq!(submenu(0.5, true).extent, 1.0);
        assert_eq!(submenu(0.75, true).opacity, 0.75);
        assert_eq!(submenu(1.0, true), Frame::REST);
        assert_eq!(submenu(0.0, false), Frame::REST);
        assert_eq!(submenu(0.25, false).opacity, 0.25);
        assert_eq!(submenu(0.25, false).extent, 1.0);
        assert_eq!(submenu(0.75, false).extent, 0.25);
        assert_eq!(submenu(1.0, false).extent, 0.0);
    }

    #[test]
    fn reduced_motion_keeps_only_the_fade() {
        let frame = banner_enter(0.5, 80.0);
        let faded = frame.without_motion();
        assert_eq!(faded.opacity, frame.opacity);
        assert_eq!((faded.dy, faded.scale, faded.extent), (0.0, 1.0, 1.0));
        assert_eq!(message(0.5, true).gated(false).extent, 1.0);
        assert_eq!(message(0.5, true).gated(true), message(0.5, true));
    }

    #[test]
    fn fades_ease_out_quad_from_where_they_stand() {
        assert_eq!(fade(0.0, 1.0, 0.0), 0.0);
        assert_eq!(fade(0.0, 1.0, 0.5), 0.75);
        assert_eq!(fade(0.4, 0.0, 1.0), 0.0);
        assert_eq!(fade(0.5, 1.0, 0.5), 0.875);
    }
}
