//! GNOME 51's minimize and unminimize animations (windowManager.js
//! `_minimizeWindow` / `_unminimizeWindow`): 400 ms ease-out-expo, the
//! window scaled and moved into the icon the shell published for it
//! (`SetIconGeometries`, Mutter's `set_icon_geometry`), fading out on the
//! way down; without an icon it heads to the monitor's top-left corner
//! (top-right right to left) at scale 0. A window covering its monitor,
//! or reduced motion, only fades.
//!
//! Purely a render-time transform: the window manager hides or restores
//! the window at once (focus and input never wait), and this module only
//! says where the frame draws it until the animation settles. A
//! direction change mid-flight starts from the pose currently drawn, so
//! the motion reverses without a jump.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use tuna_shell_control::MotionPolicy;

use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};

/// GNOME's `MINIMIZE_WINDOW_ANIMATION_TIME`.
pub const DURATION_MS: f64 = 400.0;

/// Highest opacity an animating window is drawn at. Smithay reports no
/// opaque region for a surface drawn below full opacity, so the occlusion
/// pass (#503) never culls what lies beneath a window that is shrinking,
/// moving or fading through the frame; the difference is under a tenth
/// of one 8-bit step.
const ANIMATING_ALPHA_MAX: f64 = 0.999;

/// Settled animations kept for introspection (journeys only).
const SETTLED_KEPT: usize = 8;

type Rect = Rectangle<i32, Logical>;

/// One window's surface tree as drawn by the frame (the runtime's
/// preview element type).
pub type Element = RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>;

/// How much motion the session allows (#493): full scales, moves and
/// fades; fade-only (reduced motion) only fades; off snaps.
pub use tuna_shell_control::MotionLevel as Motion;

/// Which way the window goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Minimize,
    Restore,
}

impl Direction {
    fn name(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Restore => "restore",
        }
    }
}

/// Where the window's visible rectangle is drawn, and how opaque.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub alpha: f64,
}

impl Pose {
    fn of(rect: Rect, alpha: f64) -> Self {
        Self {
            x: f64::from(rect.loc.x),
            y: f64::from(rect.loc.y),
            width: f64::from(rect.size.w),
            height: f64::from(rect.size.h),
            alpha,
        }
    }

    /// Moved `dx` along and faded by `alpha`: a workspace switch
    /// carrying the window.
    pub fn slid(self, dx: f64, alpha: f64) -> Self {
        Self {
            x: self.x + dx,
            alpha: self.alpha * alpha,
            ..self
        }
    }

    fn lerp(self, to: Self, p: f64) -> Self {
        let l = |a: f64, b: f64| a + (b - a) * p;
        Self {
            x: l(self.x, to.x),
            y: l(self.y, to.y),
            width: l(self.width, to.width),
            height: l(self.height, to.height),
            alpha: l(self.alpha, to.alpha),
        }
    }
}

/// Clutter's `CLUTTER_EASE_OUT_EXPO` over progress `p` in 0..=1.
pub fn ease_out_expo(p: f64) -> f64 {
    if p >= 1.0 {
        1.0
    } else if p <= 0.0 {
        0.0
    } else {
        1.0 - 2f64.powf(-10.0 * p)
    }
}

/// The start and end poses GNOME eases between, or `None` when nothing
/// animates. `window` is the window's rectangle, `icon` its published
/// icon, `monitor` the output it sits on.
pub fn plan(
    direction: Direction,
    motion: Motion,
    window: Rect,
    icon: Option<Rect>,
    monitor: Rect,
    right_to_left: bool,
) -> Option<(Pose, Pose)> {
    let shown = Pose::of(window, 1.0);
    let hidden = |alpha| {
        // Mutter's `is_monitor_sized` windows (and reduced motion) only
        // fade: shrinking them reads as the whole screen collapsing.
        if motion == Motion::FadeOnly || window.contains_rect(monitor) {
            return Pose {
                alpha: 0.0,
                ..shown
            };
        }
        match icon {
            Some(icon) => Pose::of(icon, alpha),
            None => {
                let x = monitor.loc.x + if right_to_left { monitor.size.w } else { 0 };
                Pose::of(Rect::new((x, monitor.loc.y).into(), (0, 0).into()), alpha)
            }
        }
    };
    match (motion, direction) {
        (Motion::Off, _) => None,
        (_, Direction::Minimize) => Some((shown, hidden(0.0))),
        // GNOME restores at full opacity: only the minimize fades.
        (_, Direction::Restore) => Some((hidden(1.0), shown)),
    }
}

struct Animation<S> {
    direction: Direction,
    surface: S,
    from: Pose,
    to: Pose,
    start: Duration,
    duration: Duration,
}

impl<S> Animation<S> {
    fn progress(&self, now: Duration) -> f64 {
        let t = now.saturating_sub(self.start).as_secs_f64();
        let d = self.duration.as_secs_f64();
        if d <= 0.0 {
            1.0
        } else {
            (t / d).min(1.0)
        }
    }

    fn pose(&self, now: Duration) -> Pose {
        self.from.lerp(self.to, ease_out_expo(self.progress(now)))
    }
}

/// One finished animation, for the journey state file.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub window: u64,
    pub direction: Direction,
    /// Whether it animated at all (motion off snaps).
    pub animated: bool,
    /// Animation-clock time from start to the frame that settled it.
    pub elapsed_ms: f64,
    /// Where it started and ended.
    pub from: Pose,
    pub to: Pose,
}

/// Every running minimize and restore animation, keyed by window id.
/// `S` is the window's surface (generic only so tests need no client).
pub struct MinimizeAnimations<S = WlSurface> {
    /// Hide and restore transitions the window manager made since the
    /// last frame, oldest first.
    pending: Vec<(u64, Direction)>,
    running: HashMap<u64, Animation<S>>,
    settled: VecDeque<Settled>,
    /// Frame time on the runtime's animation clock: every output of one
    /// frame draws the same instant.
    now: Duration,
    motion: MotionPolicy,
}

impl<S> Default for MinimizeAnimations<S> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            running: HashMap::new(),
            settled: VecDeque::new(),
            now: Duration::ZERO,
            motion: MotionPolicy::default(),
        }
    }
}

/// What the runtime knows about a window when its animation starts.
pub struct Start<S = WlSurface> {
    pub surface: S,
    pub window: Rect,
    pub icon: Option<Rect>,
    pub monitor: Rect,
    pub right_to_left: bool,
}

impl<S: PartialEq> MinimizeAnimations<S> {
    /// The window manager hid (`Minimize`) or restored `id`.
    pub fn queue(&mut self, id: u64, direction: Direction) {
        self.pending.push((id, direction));
    }

    /// The session's motion policy and GNOME's slow-down factor.
    pub fn set_motion(&mut self, motion: MotionPolicy) {
        self.motion = motion;
        if motion.level == Motion::Off {
            let now = self.now;
            for id in self.running.keys().copied().collect::<Vec<_>>() {
                self.finish(id, now);
            }
        }
    }

    /// Advance to the frame at `now`: start the queued transitions
    /// (`start` describes a window, `None` once it is gone), settle the
    /// finished ones and drop those whose window closed (`alive`).
    /// Returns whether anything is still moving. Costs nothing while
    /// idle: no queue, no running animation.
    pub fn step(
        &mut self,
        now: Duration,
        start: impl Fn(u64) -> Option<Start<S>>,
        alive: impl Fn(u64) -> bool,
    ) -> bool {
        self.now = now;
        if self.pending.is_empty() && self.running.is_empty() {
            return false;
        }
        for (id, direction) in std::mem::take(&mut self.pending) {
            let Some(start) = start(id) else {
                self.running.remove(&id);
                continue;
            };
            self.begin(id, direction, start, now);
        }
        let done: Vec<u64> = self
            .running
            .iter()
            .filter(|(_, a)| a.progress(now) >= 1.0)
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            self.finish(id, now);
        }
        self.running.retain(|id, _| alive(*id));
        !self.running.is_empty()
    }

    fn begin(&mut self, id: u64, direction: Direction, start: Start<S>, now: Duration) {
        let planned = plan(
            direction,
            self.motion.level,
            start.window,
            start.icon,
            start.monitor,
            start.right_to_left,
        );
        let Some((from, to)) = planned else {
            self.running.remove(&id);
            self.record(Settled {
                window: id,
                direction,
                animated: false,
                elapsed_ms: 0.0,
                from: Pose::of(start.window, 1.0),
                to: Pose::of(start.window, 1.0),
            });
            return;
        };
        // Mid-flight: carry on from what is drawn now, so a reversal
        // never jumps.
        let from = self
            .running
            .get(&id)
            .map(|running| running.pose(now))
            .unwrap_or(from);
        self.running.insert(
            id,
            Animation {
                direction,
                surface: start.surface,
                from,
                to,
                start: now,
                duration: Duration::from_secs_f64(self.motion.adjust_ms(DURATION_MS) / 1000.0),
            },
        );
    }

    fn finish(&mut self, id: u64, now: Duration) {
        if let Some(a) = self.running.remove(&id) {
            self.record(Settled {
                window: id,
                direction: a.direction,
                animated: true,
                elapsed_ms: now.saturating_sub(a.start).as_secs_f64() * 1000.0,
                from: a.from,
                to: a.to,
            });
        }
    }

    fn record(&mut self, settled: Settled) {
        if self.settled.len() == SETTLED_KEPT {
            self.settled.pop_front();
        }
        self.settled.push_back(settled);
    }

    /// Whether `id` is animating.
    pub fn is_animating(&self, id: u64) -> bool {
        self.running.contains_key(&id)
    }

    /// Where a restoring (still mapped) window's surface draws this
    /// frame, if it is animating.
    pub fn pose_of_surface(&self, surface: &S) -> Option<Pose> {
        self.running
            .values()
            .find(|a| a.surface == *surface)
            .map(|a| a.pose(self.now))
    }

    /// Windows on their way down: the manager already hides them, so
    /// the frame draws them from here.
    pub fn minimizing(&self) -> impl Iterator<Item = (u64, &S, Pose)> + '_ {
        self.running
            .iter()
            .filter(|(_, a)| a.direction == Direction::Minimize)
            .map(|(id, a)| (*id, &a.surface, a.pose(self.now)))
    }

    /// Running animations as `(window, direction, pose, progress)`.
    pub fn running(&self) -> Vec<(u64, &'static str, Pose, f64)> {
        let mut out: Vec<_> = self
            .running
            .iter()
            .map(|(id, a)| {
                (
                    *id,
                    a.direction.name(),
                    a.pose(self.now),
                    a.progress(self.now),
                )
            })
            .collect();
        out.sort_by_key(|(id, ..)| *id);
        out
    }

    /// The last few settled animations, oldest first.
    pub fn settled(&self) -> impl Iterator<Item = &Settled> {
        self.settled.iter()
    }
}

impl Settled {
    /// The direction as the state file names it.
    pub fn direction_name(&self) -> &'static str {
        self.direction.name()
    }
}

/// The surface tree of a window whose visible rectangle is `geometry`,
/// mapped onto `pose`, popups included: scaled about the visible
/// top-left and moved there, at the pose's opacity. Bottom to top.
pub fn elements(
    renderer: &mut GlesRenderer,
    view: crate::runtime::View,
    surface: &WlSurface,
    geometry: Rect,
    pose: Pose,
) -> Vec<Element> {
    if pose.alpha <= 0.0 || geometry.size.w <= 0 || geometry.size.h <= 0 {
        return Vec::new();
    }
    let scale = (
        pose.width / f64::from(geometry.size.w),
        pose.height / f64::from(geometry.size.h),
    );
    if scale.0 <= 0.0 || scale.1 <= 0.0 {
        return Vec::new();
    }
    let pivot = view.physical(pose.x, pose.y);
    let shift = (
        pose.x - f64::from(geometry.loc.x),
        pose.y - f64::from(geometry.loc.y),
    );
    let origin = crate::popup::surface_origin(surface, geometry.loc);
    let mut trees = vec![(surface.clone(), origin)];
    trees.extend(
        crate::popup::placed_popups(surface, origin, true)
            .into_iter()
            .map(|popup| (popup.surface, popup.origin)),
    );
    let mut out = Vec::new();
    for (tree, at) in trees {
        let at: Point<i32, Logical> = at;
        let location = view.physical(f64::from(at.x) + shift.0, f64::from(at.y) + shift.1);
        out.extend(
            render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
                renderer,
                &tree,
                location,
                view.scale,
                pose.alpha.min(ANIMATING_ALPHA_MAX) as f32,
                Kind::Unspecified,
            )
            .into_iter()
            .map(|element| RescaleRenderElement::from_element(element, pivot, scale)),
        );
    }
    out
}

/// Every window still shrinking on the active workspace (the manager
/// already hides them), bottom to top.
pub fn minimizing_elements(
    renderer: &mut GlesRenderer,
    manager: &crate::windows::WindowManager,
    view: crate::runtime::View,
) -> Vec<Element> {
    let model = manager.model();
    let active = model.active_workspace();
    let mut out = Vec::new();
    let slide = manager.workspace_slide();
    for (id, surface, pose) in manager.minimize_animations.minimizing() {
        // A workspace switch draws both workspaces, sliding (or
        // crossfading): the shrinking window goes along with its own.
        let placement = if manager.is_sticky(id) || !slide.running() {
            None
        } else {
            model
                .window(id)
                .and_then(|w| slide.frame_placement(w.workspace))
        };
        let shown = manager.is_sticky(id)
            || placement.is_some()
            || model.window(id).is_some_and(|w| w.workspace == active);
        if let Some(mut geometry) = manager.render_geometry(id).filter(|_| shown) {
            let (dx, alpha) = placement.map_or((0, 1.0), |p| (p.dx, p.alpha));
            geometry.loc.x += dx;
            let pose = pose.slid(f64::from(dx), f64::from(alpha));
            out.extend(elements(renderer, view, surface, geometry, pose));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y).into(), (w, h).into())
    }
    const MONITOR: (i32, i32, i32, i32) = (0, 0, 1280, 800);
    fn monitor() -> Rect {
        rect(MONITOR.0, MONITOR.1, MONITOR.2, MONITOR.3)
    }

    #[test]
    fn ease_out_expo_matches_clutter() {
        assert_eq!(ease_out_expo(0.0), 0.0);
        assert_eq!(ease_out_expo(1.0), 1.0);
        // -2^(-10 t) + 1: half way is already 1 - 1/32.
        assert!((ease_out_expo(0.5) - (1.0 - 1.0 / 32.0)).abs() < 1e-12);
        assert!((ease_out_expo(0.1) - 0.5).abs() < 1e-12);
        let mut last = 0.0;
        for i in 1..=100 {
            let v = ease_out_expo(f64::from(i) / 100.0);
            assert!(v > last, "monotonic at {i}");
            last = v;
        }
        assert_eq!(ease_out_expo(-1.0), 0.0);
        assert_eq!(ease_out_expo(2.0), 1.0);
    }

    #[test]
    fn minimize_heads_into_the_icon_and_fades() {
        let window = rect(200, 100, 640, 480);
        let icon = rect(600, 712, 76, 76);
        let (from, to) = plan(
            Direction::Minimize,
            Motion::Full,
            window,
            Some(icon),
            monitor(),
            false,
        )
        .unwrap();
        assert_eq!(from, Pose::of(window, 1.0));
        assert_eq!(to, Pose::of(icon, 0.0));
    }

    #[test]
    fn restore_grows_out_of_the_icon_at_full_opacity() {
        let window = rect(200, 100, 640, 480);
        let icon = rect(600, 712, 76, 76);
        let (from, to) = plan(
            Direction::Restore,
            Motion::Full,
            window,
            Some(icon),
            monitor(),
            false,
        )
        .unwrap();
        assert_eq!(from, Pose::of(icon, 1.0));
        assert_eq!(to, Pose::of(window, 1.0));
    }

    #[test]
    fn without_an_icon_the_monitor_corner_by_text_direction() {
        let window = rect(200, 100, 640, 480);
        let second = rect(1280, 0, 1920, 1080);
        let (_, ltr) = plan(
            Direction::Minimize,
            Motion::Full,
            window,
            None,
            second,
            false,
        )
        .unwrap();
        assert_eq!(ltr, Pose::of(rect(1280, 0, 0, 0), 0.0));
        let (_, rtl) = plan(
            Direction::Minimize,
            Motion::Full,
            window,
            None,
            second,
            true,
        )
        .unwrap();
        assert_eq!(rtl, Pose::of(rect(3200, 0, 0, 0), 0.0));
        let (from, _) = plan(Direction::Restore, Motion::Full, window, None, second, true).unwrap();
        assert_eq!(from, Pose::of(rect(3200, 0, 0, 0), 1.0));
    }

    #[test]
    fn monitor_sized_windows_and_fade_only_motion_only_fade() {
        let icon = Some(rect(600, 712, 76, 76));
        let full = monitor();
        let (from, to) = plan(
            Direction::Minimize,
            Motion::Full,
            full,
            icon,
            monitor(),
            false,
        )
        .unwrap();
        assert_eq!((from, to), (Pose::of(full, 1.0), Pose::of(full, 0.0)));
        let window = rect(200, 100, 640, 480);
        let (from, to) = plan(
            Direction::Minimize,
            Motion::FadeOnly,
            window,
            icon,
            monitor(),
            false,
        )
        .unwrap();
        assert_eq!((from, to), (Pose::of(window, 1.0), Pose::of(window, 0.0)));
        let (from, to) = plan(
            Direction::Restore,
            Motion::FadeOnly,
            window,
            icon,
            monitor(),
            false,
        )
        .unwrap();
        assert_eq!((from, to), (Pose::of(window, 0.0), Pose::of(window, 1.0)));
    }

    #[test]
    fn motion_off_plans_nothing() {
        let window = rect(200, 100, 640, 480);
        for direction in [Direction::Minimize, Direction::Restore] {
            assert!(plan(direction, Motion::Off, window, None, monitor(), false).is_none());
        }
    }

    #[test]
    fn a_workspace_switch_carries_and_fades_the_pose() {
        let pose = Pose::of(rect(100, 200, 76, 76), 0.5);
        let slid = pose.slid(-1280.0, 0.5);
        assert_eq!(
            (slid.x, slid.y, slid.width, slid.height),
            (-1180.0, 200.0, 76.0, 76.0)
        );
        assert_eq!(slid.alpha, 0.25);
        assert_eq!(pose.slid(0.0, 1.0), pose);
    }

    #[test]
    fn pose_lerp_meets_both_ends() {
        let a = Pose::of(rect(0, 0, 100, 100), 1.0);
        let b = Pose::of(rect(100, 200, 10, 20), 0.0);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        let half = a.lerp(b, 0.5);
        assert_eq!(
            (half.x, half.y, half.width, half.height),
            (50.0, 100.0, 55.0, 60.0)
        );
        assert_eq!(half.alpha, 0.5);
    }

    fn start(window: Rect) -> Option<Start<u32>> {
        Some(Start {
            surface: 7,
            window,
            icon: Some(rect(600, 712, 76, 76)),
            monitor: monitor(),
            right_to_left: false,
        })
    }

    #[test]
    fn settles_after_400_ms_and_is_idle_after() {
        let window = rect(200, 100, 640, 480);
        let t0 = Duration::from_secs(5);
        let mut anims = MinimizeAnimations::<u32>::default();
        assert!(!anims.step(t0, |_| start(window), |_| true));
        anims.queue(1, Direction::Minimize);
        assert!(anims.step(t0, |_| start(window), |_| true));
        assert_eq!(anims.minimizing().count(), 1);
        assert!(anims.step(t0 + Duration::from_millis(200), |_| start(window), |_| true));
        let (_, _, mid) = anims.minimizing().next().unwrap();
        // Ease-out: most of the way into the icon at half time.
        assert!(
            mid.x > 500.0 && mid.y > 600.0 && mid.width < 150.0,
            "{mid:?}"
        );
        assert!(!anims.step(t0 + Duration::from_millis(416), |_| start(window), |_| true));
        let settled = anims.settled().last().unwrap();
        assert!(settled.animated);
        assert!((settled.elapsed_ms - 416.0).abs() < 1e-6);
        assert_eq!(settled.to, Pose::of(rect(600, 712, 76, 76), 0.0));
        assert!(!anims.is_animating(1));
    }

    #[test]
    fn reversing_mid_flight_starts_from_the_drawn_pose() {
        let window = rect(200, 100, 640, 480);
        let t0 = Duration::from_secs(5);
        let mut anims = MinimizeAnimations::<u32>::default();
        anims.queue(1, Direction::Minimize);
        anims.step(t0, |_| start(window), |_| true);
        let t1 = t0 + Duration::from_millis(100);
        anims.step(t1, |_| start(window), |_| true);
        let drawn = anims.pose_of_surface(&7).unwrap();
        anims.queue(1, Direction::Restore);
        assert!(anims.step(t1, |_| start(window), |_| true));
        assert_eq!(anims.pose_of_surface(&7).unwrap(), drawn);
        assert_eq!(anims.minimizing().count(), 0);
        // It then grows back to the window over a full 400 ms.
        assert!(anims.step(t1 + Duration::from_millis(399), |_| start(window), |_| true));
        assert!(!anims.step(t1 + Duration::from_millis(400), |_| start(window), |_| true));
        assert_eq!(anims.settled().last().unwrap().to, Pose::of(window, 1.0));
    }

    #[test]
    fn motion_off_snaps_and_records_no_animation() {
        let window = rect(200, 100, 640, 480);
        let t0 = Duration::from_secs(5);
        let mut anims = MinimizeAnimations::<u32>::default();
        anims.queue(1, Direction::Minimize);
        anims.step(t0, |_| start(window), |_| true);
        anims.set_motion(MotionPolicy::new(Motion::Off, 1.0));
        assert!(!anims.is_animating(1));
        anims.queue(1, Direction::Restore);
        assert!(!anims.step(t0, |_| start(window), |_| true));
        let last = anims.settled().last().unwrap();
        assert!(!last.animated);
        assert_eq!(last.direction, Direction::Restore);
    }

    #[test]
    fn slow_down_stretches_and_closed_windows_drop() {
        let window = rect(200, 100, 640, 480);
        let t0 = Duration::from_secs(5);
        let mut anims = MinimizeAnimations::<u32>::default();
        anims.set_motion(MotionPolicy::new(Motion::Full, 2.0));
        anims.queue(1, Direction::Minimize);
        anims.step(t0, |_| start(window), |_| true);
        assert!(anims.step(t0 + Duration::from_millis(450), |_| start(window), |_| true));
        // The window closed: nothing left to draw.
        assert!(!anims.step(t0 + Duration::from_millis(500), |_| None, |_| false));
        assert!(!anims.is_animating(1));
    }
}
