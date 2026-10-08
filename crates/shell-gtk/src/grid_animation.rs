//! GNOME 51's app grid animations (appDisplay.js, iconGrid.js): the
//! folder dialog's zoom from its icon, icon scale-in and scale-and-fade,
//! the launch zoom, the page switch, the page arrows' fade and the
//! staggered reflow while dragging.
//!
//! Each animation is a tick callback that runs only while it moves and
//! touches nothing but a transform and an opacity: no relayout per frame.
//! Fades and motion are separate parts, so Reduced Motion keeps the fades
//! and drops the zooms, slides and scales; with animations off all of it
//! is instant.
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The shell's effective motion policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// Everything animates.
    Full,
    /// Reduced Motion: fades only; zooms, slides and scales jump.
    FadesOnly,
    /// enable-animations off: nothing animates.
    Off,
}

impl Motion {
    pub fn from_settings(animations: bool, reduced: bool) -> Self {
        match (animations, reduced) {
            (false, _) => Motion::Off,
            (true, true) => Motion::FadesOnly,
            (true, false) => Motion::Full,
        }
    }

    pub fn allows(self, part: Part) -> bool {
        matches!(
            (self, part),
            (Motion::Full, _) | (Motion::FadesOnly, Part::Fade)
        )
    }
}

/// What an animated property is: an opacity, or a movement/scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Fade,
    Motion,
}

thread_local! {
    static POLICY: Cell<Option<Motion>> = const { Cell::new(None) };
}

/// Set from GNOME's enable-animations and Reduced Motion keys.
pub fn set_policy(motion: Motion) {
    if POLICY.with(|p| p.replace(Some(motion))) != Some(motion) {
        trace("policy", format_args!("{motion:?}"));
    }
}

/// The current policy; before the settings arrive, GTK's own switch.
pub fn motion() -> Motion {
    POLICY.with(|p| p.get()).unwrap_or_else(|| {
        let on = gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations());
        Motion::from_settings(on, false)
    })
}

/// Clutter's easing modes the grid uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    InQuad,
    OutQuad,
    OutCubic,
    OutQuint,
    OutExpo,
}

impl Curve {
    pub fn at(self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        match self {
            Curve::InQuad => p * p,
            Curve::OutQuad => 1.0 - (1.0 - p).powi(2),
            Curve::OutCubic => 1.0 - (1.0 - p).powi(3),
            Curve::OutQuint => 1.0 - (1.0 - p).powi(5),
            Curve::OutExpo => {
                if p >= 1.0 {
                    1.0
                } else {
                    1.0 - 2f64.powf(-10.0 * p)
                }
            }
        }
    }
}

/// One eased property: when it starts, how long it runs, its curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timing {
    pub delay_ms: f64,
    pub duration_ms: f64,
    pub curve: Curve,
    pub part: Part,
}

impl Timing {
    const fn new(duration_ms: f64, curve: Curve, part: Part) -> Self {
        Self {
            delay_ms: 0.0,
            duration_ms,
            curve,
            part,
        }
    }

    pub const fn delayed(self, delay_ms: f64) -> Self {
        Self { delay_ms, ..self }
    }

    pub const fn lasting(self, duration_ms: f64) -> Self {
        Self {
            duration_ms,
            ..self
        }
    }

    /// Eased progress `elapsed_ms` in; 1 when its part is not animated.
    pub fn at(&self, elapsed_ms: f64, motion: Motion) -> f64 {
        if !motion.allows(self.part) || self.duration_ms <= 0.0 {
            return 1.0;
        }
        self.curve
            .at((elapsed_ms - self.delay_ms) / self.duration_ms)
    }

    /// When it settles, 0 when it does not animate.
    pub fn end_ms(&self, motion: Motion) -> f64 {
        if motion.allows(self.part) {
            self.delay_ms + self.duration_ms
        } else {
            0.0
        }
    }
}

/// `FOLDER_DIALOG_ANIMATION_TIME`: the dialog's zoom (ease-out-expo)
/// from its icon and back.
pub const FOLDER_ZOOM: Timing = Timing::new(200.0, Curve::OutExpo, Part::Motion);
/// The dialog's own fade, with the zoom: ease-out-expo opening,
/// ease-out-quad closing.
pub const FOLDER_FADE_IN: Timing = Timing::new(200.0, Curve::OutExpo, Part::Fade);
pub const FOLDER_FADE_OUT: Timing = Timing::new(200.0, Curve::OutQuad, Part::Fade);
/// The shade behind the dialog darkening and clearing.
pub const FOLDER_SHADE: Timing = Timing::new(200.0, Curve::OutQuad, Part::Fade);
/// The folder icon in the grid hides over the first half of the
/// opening and returns over the last half of the closing.
pub const FOLDER_ICON_HIDE: Timing = Timing::new(100.0, Curve::OutQuad, Part::Fade);
pub const FOLDER_ICON_SHOW: Timing = Timing::new(100.0, Curve::InQuad, Part::Fade).delayed(100.0);
/// `APP_ICON_SCALE_IN_TIME` after `APP_ICON_SCALE_IN_DELAY`.
pub const ICON_SCALE_IN: Timing = Timing::new(500.0, Curve::OutQuint, Part::Motion).delayed(700.0);
/// scaleAndFade/undoScaleAndFade: Clutter's implicit 250 ms
/// ease-out-cubic, to half size and transparent.
pub const ICON_SCALE: Timing = Timing::new(250.0, Curve::OutCubic, Part::Motion);
pub const ICON_FADE: Timing = Timing::new(250.0, Curve::OutCubic, Part::Fade);
/// `APPICON_ANIMATION_OUT_TIME`, to `APPICON_ANIMATION_OUT_SCALE`.
pub const LAUNCH_ZOOM: Timing = Timing::new(250.0, Curve::OutQuad, Part::Motion);
pub const LAUNCH_FADE: Timing = Timing::new(250.0, Curve::OutQuad, Part::Fade);
pub const LAUNCH_SCALE: f64 = 3.0;
/// `PAGE_SWITCH_TIME`.
pub const PAGE_SWITCH: Timing = Timing::new(300.0, Curve::OutCubic, Part::Motion);
/// `PAGE_INDICATOR_FADE_TIME`, Clutter's implicit ease-out-cubic.
pub const PAGE_ARROW_FADE: Timing = Timing::new(200.0, Curve::OutCubic, Part::Fade);
/// An icon's reflow: Clutter's implicit 250 ms, ease-out-quad, each
/// moved icon `ICON_POSITION_DELAY` after the one before.
pub const REFLOW: Timing = Timing::new(250.0, Curve::OutQuad, Part::Motion);
pub const REFLOW_STAGGER_MS: f64 = 10.0;

pub fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

/// The launch clone's box at `t`: (center x, center y, scale) of an icon
/// centred at `center`, `size` square, grown `LAUNCH_SCALE` times while
/// staying inside `bounds` (x, y, width, height), as zoomOutActorAtPos
/// keeps it on the monitor.
pub fn launch_box(
    center: (f64, f64),
    size: f64,
    bounds: (f64, f64, f64, f64),
    t: f64,
) -> (f64, f64, f64) {
    let target = launch_center(center, size, bounds);
    (
        lerp(center.0, target.0, t),
        lerp(center.1, target.1, t),
        lerp(1.0, LAUNCH_SCALE, t),
    )
}

/// Where the fully grown launch clone is centred.
pub fn launch_center(center: (f64, f64), size: f64, bounds: (f64, f64, f64, f64)) -> (f64, f64) {
    let grown = size * LAUNCH_SCALE;
    let contain = |c: f64, lo: f64, span: f64| {
        let start = c - grown / 2.0;
        start.clamp(lo, (lo + span - grown).max(lo)) + grown / 2.0
    };
    (
        contain(center.0, bounds.0, bounds.2),
        contain(center.1, bounds.1, bounds.3),
    )
}

/// The folder dialog's zoom at `t` (0 at the icon, 1 in place): its
/// top-left offset from its resting place and its scale, as GNOME's
/// translation and scale with the pivot at the top-left corner.
pub fn folder_zoom(
    source: (f64, f64, f64, f64),
    dialog: (f64, f64, f64, f64),
    t: f64,
) -> ((f64, f64), (f64, f64)) {
    let (sx, sy, sw, sh) = source;
    let (dx, dy, dw, dh) = dialog;
    (
        (lerp(sx - dx, 0.0, t), lerp(sy - dy, 0.0, t)),
        (lerp(sw / dw, 1.0, t), lerp(sh / dh, 1.0, t)),
    )
}

/// How long a switch of `distance` pages takes: GNOME's 300 ms for a
/// whole page, less for a swipe already part of the way there.
pub fn page_switch_ms(distance: f64) -> f64 {
    PAGE_SWITCH.duration_ms * distance.abs().clamp(0.0, 1.0).max(0.25)
}

/// The slot a tile moves by in a reflow, in cells: (columns, rows).
pub fn reflow_shift(from: usize, to: usize, columns: usize) -> (f64, f64) {
    let columns = columns.max(1);
    (
        (from % columns) as f64 - (to % columns) as f64,
        (from / columns) as f64 - (to / columns) as f64,
    )
}

fn trace(name: &str, what: std::fmt::Arguments) {
    if std::env::var_os("TUNA_GRID_TRACE").is_some() {
        eprintln!("tuna-shell-gtk: grid-anim {name} {what}");
    }
}

/// One running animation per property set: a new start replaces it.
#[derive(Default)]
pub struct Slot {
    tick: RefCell<Option<gtk::TickCallbackId>>,
}

impl Slot {
    pub fn stop(&self) {
        if let Some(tick) = self.tick.borrow_mut().take() {
            tick.remove();
        }
    }

    pub fn running(&self) -> bool {
        self.tick.borrow().is_some()
    }

    /// Run `step` with the elapsed milliseconds on `widget`'s frames for
    /// `total_ms` (from `parts` under `motion`), then `done`. With
    /// nothing to animate, one final step and `done` happen at once.
    pub fn start(
        self: &Rc<Self>,
        widget: &impl IsA<gtk::Widget>,
        name: &'static str,
        total_ms: f64,
        step: impl Fn(f64) + 'static,
        done: impl FnOnce() + 'static,
    ) {
        self.stop();
        if total_ms <= 0.0 {
            step(f64::INFINITY);
            trace(name, format_args!("instant motion={:?}", motion()));
            done();
            return;
        }
        step(0.0);
        trace(
            name,
            format_args!("start ms={total_ms:.0} motion={:?}", motion()),
        );
        let started = Cell::new(None::<i64>);
        let done = RefCell::new(Some(done));
        let weak = Rc::downgrade(self);
        let id = widget.add_tick_callback(move |_, clock| {
            let now = clock.frame_time();
            let start = *started.get().get_or_insert(now);
            started.set(Some(start));
            let elapsed = (now - start) as f64 / 1000.0;
            step(elapsed);
            if elapsed < total_ms {
                return glib::ControlFlow::Continue;
            }
            trace(name, format_args!("settle ms={elapsed:.0}"));
            if let Some(slot) = weak.upgrade() {
                // Returning Break removes the callback; forget its id.
                if let Some(id) = slot.tick.borrow_mut().take() {
                    std::mem::forget(id);
                }
            }
            if let Some(done) = done.borrow_mut().take() {
                done();
            }
            glib::ControlFlow::Break
        });
        *self.tick.borrow_mut() = Some(id);
    }
}

/// Fade `widget` in or out over `PAGE_ARROW_FADE`, hiding it once out.
/// While fading out it takes no clicks. Unmapped, it jumps.
pub fn fade_visible(widget: &gtk::Widget, slot: &Rc<Slot>, name: &'static str, visible: bool) {
    let shown = widget.parent().is_some_and(|parent| parent.is_mapped());
    let animate = shown && widget.is_visible() != visible;
    let from = if widget.is_visible() {
        widget.opacity()
    } else {
        0.0
    };
    let to = if visible { 1.0 } else { 0.0 };
    if visible {
        widget.set_visible(true);
    }
    widget.set_can_target(visible);
    let total = if animate || slot.running() {
        PAGE_ARROW_FADE.end_ms(motion())
    } else {
        0.0
    };
    let policy = motion();
    let w = widget.downgrade();
    let step = move |elapsed: f64| {
        if let Some(w) = w.upgrade() {
            w.set_opacity(lerp(from, to, PAGE_ARROW_FADE.at(elapsed, policy)));
        }
    };
    let w = widget.downgrade();
    slot.start(widget, name, total, step, move || {
        if let Some(w) = w.upgrade() {
            if !visible {
                w.set_visible(false);
            }
        }
    });
}

/// Where a layer window's (0, 0) is on the output.
fn window_origin(window: &gtk::Window) -> (f64, f64) {
    use gtk4_layer_shell::{Edge, LayerShell};
    if !window.is_layer_window() {
        return (0.0, 0.0);
    }
    let along = |edge: Edge| {
        if window.is_anchor(edge) {
            f64::from(window.margin(edge))
        } else {
            0.0
        }
    };
    (along(Edge::Left), along(Edge::Top))
}

/// `widget`'s box on the output: (x, y, width, height).
pub fn screen_bounds(widget: &impl IsA<gtk::Widget>) -> Option<(f64, f64, f64, f64)> {
    let window = widget.root()?.downcast::<gtk::Window>().ok()?;
    let bounds = widget.compute_bounds(&window)?;
    let (ox, oy) = window_origin(&window);
    Some((
        ox + f64::from(bounds.x()),
        oy + f64::from(bounds.y()),
        f64::from(bounds.width()),
        f64::from(bounds.height()),
    ))
}

/// The first output's box.
pub fn monitor_bounds() -> (f64, f64, f64, f64) {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_downcast::<gtk::gdk::Monitor>()
        .map(|m| {
            let g = m.geometry();
            (
                f64::from(g.x()),
                f64::from(g.y()),
                f64::from(g.width()),
                f64::from(g.height()),
            )
        })
        .unwrap_or((0.0, 0.0, 1280.0, 800.0))
}

fn first_image(widget: &gtk::Widget) -> Option<gtk::Image> {
    let mut pending = vec![widget.clone()];
    while let Some(widget) = pending.pop() {
        if let Ok(image) = widget.clone().downcast::<gtk::Image>() {
            return Some(image);
        }
        let mut child = widget.last_child();
        while let Some(next) = child {
            child = next.prev_sibling();
            pending.push(next);
        }
    }
    None
}

/// GNOME's launch animation (iconGrid.js zoomOutActor): a copy of the
/// launched tile's icon grows to three times its size and fades out over
/// 250 ms, kept on the monitor, while the overview closes. It is drawn
/// on its own click-through overlay surface, only as big as the grown
/// icon, and destroyed when done.
pub fn launch_zoom(tile: &impl IsA<gtk::Widget>) {
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    let policy = motion();
    let total = LAUNCH_ZOOM.end_ms(policy).max(LAUNCH_FADE.end_ms(policy));
    let Some(source) = first_image(tile.upcast_ref()) else {
        return;
    };
    let (Some((x, y, w, h)), Some(app)) = (
        screen_bounds(&source),
        source
            .root()
            .and_downcast::<gtk::Window>()
            .and_then(|window| window.application()),
    ) else {
        return;
    };
    if total <= 0.0 {
        trace("launch-zoom", format_args!("instant motion={policy:?}"));
        return;
    }
    let size = f64::from(source.pixel_size().max(16));
    let center = (x + w / 2.0, y + h / 2.0);
    let monitor = monitor_bounds();
    let grown = size * LAUNCH_SCALE;
    let end = launch_center(center, size, monitor);
    // The surface covers where the icon starts and where it ends.
    let left = (center.0 - size / 2.0).min(end.0 - grown / 2.0).floor();
    let top = (center.1 - size / 2.0).min(end.1 - grown / 2.0).floor();
    let right = (center.0 + size / 2.0).max(end.0 + grown / 2.0).ceil();
    let bottom = (center.1 + size / 2.0).max(end.1 + grown / 2.0).ceil();
    let image = match source.storage_type() {
        gtk::ImageType::IconName => {
            gtk::Image::from_icon_name(&source.icon_name().unwrap_or_default())
        }
        gtk::ImageType::Gicon => match source.gicon() {
            Some(icon) => gtk::Image::from_gicon(&icon),
            None => return,
        },
        gtk::ImageType::Paintable => gtk::Image::from_paintable(source.paintable().as_ref()),
        _ => return,
    };
    // Drawn at its grown size and scaled down: sharp at the end.
    image.set_pixel_size(grown as i32);
    image.set_size_request(grown as i32, grown as i32);
    image.set_halign(gtk::Align::Start);
    image.set_valign(gtk::Align::Start);
    let zoom = Zoom::new(&image);
    zoom.set_pivot(0.0, 0.0);
    let window = gtk::Window::new();
    window.set_application(Some(&app));
    window.add_css_class("tuna-launch-zoom");
    window.set_title(Some("Launch"));
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("tuna-shell-launch-zoom"));
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_margin(Edge::Left, (left - monitor.0) as i32);
    window.set_margin(Edge::Top, (top - monitor.1) as i32);
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_can_target(false);
    window.set_default_size((right - left) as i32, (bottom - top) as i32);
    window.set_size_request((right - left) as i32, (bottom - top) as i32);
    window.set_child(Some(&zoom));
    window.connect_realize(|window| {
        if let Some(surface) = window.surface() {
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
        }
    });
    let place = {
        let zoom = zoom.downgrade();
        move |elapsed: f64| {
            let Some(zoom) = zoom.upgrade() else { return };
            let (cx, cy, scale) =
                launch_box(center, size, monitor, LAUNCH_ZOOM.at(elapsed, policy));
            // The grown image at the surface's top-left, scaled down
            // to `scale` icons and centred on the clone's centre.
            let k = scale / LAUNCH_SCALE;
            zoom.set(
                (cx - left - grown * k / 2.0, cy - top - grown * k / 2.0),
                (k, k),
                1.0 - LAUNCH_FADE.at(elapsed, policy),
            );
        }
    };
    let slot = Rc::new(Slot::default());
    let keep = slot.clone();
    let closing = window.downgrade();
    slot.start(&zoom, "launch-zoom", total, place, move || {
        drop(keep);
        if let Some(window) = closing.upgrade() {
            window.destroy();
        }
    });
    window.present();
}

mod zoom_imp {
    use super::*;

    pub struct Zoom {
        pub shift: Cell<(f64, f64)>,
        pub scale: Cell<(f64, f64)>,
        /// Fraction of the size the scale grows from: (0, 0) top-left.
        pub pivot: Cell<(f64, f64)>,
        pub alpha: Cell<f64>,
    }

    impl Default for Zoom {
        fn default() -> Self {
            Self {
                shift: Cell::new((0.0, 0.0)),
                scale: Cell::new((1.0, 1.0)),
                pivot: Cell::new((0.5, 0.5)),
                alpha: Cell::new(1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Zoom {
        const NAME: &'static str = "TunaGridZoom";
        type Type = super::Zoom;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for Zoom {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Zoom {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let (dx, dy) = self.shift.get();
            let (sx, sy) = self.scale.get();
            let alpha = self.alpha.get();
            if alpha <= 0.0 || sx <= 0.0 || sy <= 0.0 {
                return;
            }
            if (dx, dy) == (0.0, 0.0) && (sx, sy) == (1.0, 1.0) && alpha >= 1.0 {
                self.parent_snapshot(snapshot);
                return;
            }
            let obj = self.obj();
            let (px, py) = self.pivot.get();
            let (px, py) = (px * f64::from(obj.width()), py * f64::from(obj.height()));
            if alpha < 1.0 {
                snapshot.push_opacity(alpha);
            }
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(
                (dx + px) as f32,
                (dy + py) as f32,
            ));
            snapshot.scale(sx as f32, sy as f32);
            snapshot.translate(&gtk::graphene::Point::new(-px as f32, -py as f32));
            self.parent_snapshot(snapshot);
            snapshot.restore();
            if alpha < 1.0 {
                snapshot.pop();
            }
        }
    }
}

glib::wrapper! {
    /// Draws its one child moved, scaled and faded, without relayout.
    pub struct Zoom(ObjectSubclass<zoom_imp::Zoom>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Zoom {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let zoom = glib::Object::new::<Self>();
        child.set_parent(&zoom);
        zoom
    }

    pub fn set_pivot(&self, x: f64, y: f64) {
        self.imp().pivot.set((x, y));
    }

    pub fn set(&self, shift: (f64, f64), scale: (f64, f64), alpha: f64) {
        let imp = self.imp();
        if imp.shift.replace(shift) != shift
            || imp.scale.replace(scale) != scale
            || imp.alpha.replace(alpha) != alpha
        {
            self.queue_draw();
        }
    }

    /// Only the offset (a reflow), keeping scale and opacity.
    pub fn set_shift(&self, shift: (f64, f64)) {
        if self.imp().shift.replace(shift) != shift {
            self.queue_draw();
        }
    }

    /// Only the scale (about the pivot) and opacity, keeping the offset.
    pub fn set_look(&self, scale: f64, alpha: f64) {
        let imp = self.imp();
        if imp.scale.replace((scale, scale)) != (scale, scale) || imp.alpha.replace(alpha) != alpha
        {
            self.queue_draw();
        }
    }

    pub fn shift(&self) -> (f64, f64) {
        self.imp().shift.get()
    }

    pub fn scale(&self) -> (f64, f64) {
        self.imp().scale.get()
    }

    pub fn alpha(&self) -> f64 {
        self.imp().alpha.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn curves_match_clutter_easing_modes() {
        for curve in [
            Curve::InQuad,
            Curve::OutQuad,
            Curve::OutCubic,
            Curve::OutQuint,
            Curve::OutExpo,
        ] {
            assert_eq!(curve.at(0.0), 0.0, "{curve:?}");
            assert_eq!(curve.at(1.0), 1.0, "{curve:?}");
            assert_eq!(curve.at(2.0), 1.0, "{curve:?} clamps");
        }
        assert!(close(Curve::InQuad.at(0.5), 0.25));
        assert!(close(Curve::OutQuad.at(0.5), 0.75));
        assert!(close(Curve::OutCubic.at(0.5), 0.875));
        assert!(close(Curve::OutQuint.at(0.5), 0.96875));
        assert!(close(Curve::OutExpo.at(0.5), 1.0 - 1.0 / 32.0));
    }

    #[test]
    fn gnome_51_durations() {
        assert_eq!(FOLDER_ZOOM.duration_ms, 200.0);
        assert_eq!(FOLDER_ZOOM.curve, Curve::OutExpo);
        assert_eq!(FOLDER_SHADE.duration_ms, 200.0);
        assert_eq!(FOLDER_ICON_HIDE.end_ms(Motion::Full), 100.0);
        assert_eq!(FOLDER_ICON_SHOW.end_ms(Motion::Full), 200.0);
        assert_eq!(FOLDER_ICON_SHOW.curve, Curve::InQuad);
        assert_eq!(ICON_SCALE_IN.duration_ms, 500.0);
        assert_eq!(ICON_SCALE_IN.delay_ms, 700.0);
        assert_eq!(ICON_SCALE_IN.curve, Curve::OutQuint);
        assert_eq!(LAUNCH_ZOOM.duration_ms, 250.0);
        assert_eq!(LAUNCH_ZOOM.curve, Curve::OutQuad);
        assert_eq!(PAGE_SWITCH.duration_ms, 300.0);
        assert_eq!(PAGE_SWITCH.curve, Curve::OutCubic);
        assert_eq!(PAGE_ARROW_FADE.duration_ms, 200.0);
        assert_eq!(REFLOW.curve, Curve::OutQuad);
    }

    #[test]
    fn reduced_motion_keeps_fades_and_off_keeps_nothing() {
        assert_eq!(Motion::from_settings(true, false), Motion::Full);
        assert_eq!(Motion::from_settings(true, true), Motion::FadesOnly);
        assert_eq!(Motion::from_settings(false, true), Motion::Off);
        assert_eq!(Motion::from_settings(false, false), Motion::Off);
        // Halfway through, a fade is still moving under Reduced Motion
        // while the zoom has already landed; with animations off both
        // have.
        assert!(FOLDER_SHADE.at(100.0, Motion::FadesOnly) < 1.0);
        assert_eq!(FOLDER_ZOOM.at(100.0, Motion::FadesOnly), 1.0);
        assert_eq!(FOLDER_SHADE.at(0.0, Motion::Off), 1.0);
        assert_eq!(FOLDER_ZOOM.end_ms(Motion::FadesOnly), 0.0);
        assert_eq!(FOLDER_SHADE.end_ms(Motion::Off), 0.0);
        assert_eq!(PAGE_SWITCH.at(0.0, Motion::FadesOnly), 1.0);
    }

    #[test]
    fn delays_hold_the_start_value() {
        assert_eq!(ICON_SCALE_IN.at(0.0, Motion::Full), 0.0);
        assert_eq!(ICON_SCALE_IN.at(700.0, Motion::Full), 0.0);
        assert!(close(
            ICON_SCALE_IN.at(950.0, Motion::Full),
            Curve::OutQuint.at(0.5)
        ));
        assert_eq!(ICON_SCALE_IN.at(1200.0, Motion::Full), 1.0);
        assert_eq!(FOLDER_ICON_SHOW.at(50.0, Motion::Full), 0.0);
        assert!(close(FOLDER_ICON_SHOW.at(150.0, Motion::Full), 0.25));
    }

    #[test]
    fn folder_dialog_zooms_from_its_icon() {
        let source = (271.0, 456.0, 113.0, 113.0);
        let dialog = (279.0, 56.0, 722.0, 720.0);
        let (shift, scale) = folder_zoom(source, dialog, 0.0);
        assert_eq!(shift, (-8.0, 400.0));
        assert!(close(scale.0, 113.0 / 722.0) && close(scale.1, 113.0 / 720.0));
        assert_eq!(folder_zoom(source, dialog, 1.0), ((0.0, 0.0), (1.0, 1.0)));
        let ((x, y), _) = folder_zoom(source, dialog, 0.5);
        assert_eq!((x, y), (-4.0, 200.0));
    }

    #[test]
    fn launch_zoom_triples_and_stays_on_the_monitor() {
        let monitor = (0.0, 0.0, 1280.0, 800.0);
        assert_eq!(
            launch_box((640.0, 400.0), 64.0, monitor, 0.0),
            (640.0, 400.0, 1.0)
        );
        assert_eq!(
            launch_box((640.0, 400.0), 64.0, monitor, 1.0),
            (640.0, 400.0, 3.0)
        );
        // Near the left edge the grown clone slides right to stay whole.
        assert_eq!(launch_center((40.0, 400.0), 64.0, monitor), (96.0, 400.0));
        assert_eq!(
            launch_center((1270.0, 790.0), 64.0, monitor),
            (1184.0, 704.0)
        );
        let (x, _, s) = launch_box((40.0, 400.0), 64.0, monitor, 0.5);
        assert_eq!((x, s), (68.0, 2.0));
    }

    fn page_position(from: f64, to: f64, elapsed_ms: f64, motion: Motion) -> f64 {
        lerp(from, to, PAGE_SWITCH.at(elapsed_ms, motion))
    }

    #[test]
    fn page_switch_eases_out_cubic_over_300_ms() {
        assert_eq!(page_position(0.0, 1.0, 0.0, Motion::Full), 0.0);
        assert!(close(page_position(0.0, 1.0, 150.0, Motion::Full), 0.875));
        assert_eq!(page_position(0.0, 1.0, 300.0, Motion::Full), 1.0);
        assert_eq!(page_position(2.0, 1.0, 150.0, Motion::Full), 1.125);
        assert_eq!(page_position(0.0, 1.0, 0.0, Motion::FadesOnly), 1.0);
        assert_eq!(page_position(0.0, 1.0, 0.0, Motion::Off), 1.0);
        assert_eq!(page_switch_ms(1.0), 300.0);
        assert_eq!(page_switch_ms(-0.5), 150.0);
        assert_eq!(page_switch_ms(0.01), 75.0);
    }

    #[test]
    fn reflow_moves_by_whole_cells_and_staggers() {
        assert_eq!(reflow_shift(0, 1, 8), (-1.0, 0.0));
        assert_eq!(reflow_shift(7, 8, 8), (7.0, -1.0));
        assert_eq!(reflow_shift(9, 2, 8), (-1.0, 1.0));
        let third = REFLOW.delayed(2.0 * REFLOW_STAGGER_MS);
        assert_eq!(third.at(20.0, Motion::Full), 0.0);
        assert!(close(third.at(145.0, Motion::Full), 0.75));
        assert_eq!(third.end_ms(Motion::Full), 270.0);
    }
}
