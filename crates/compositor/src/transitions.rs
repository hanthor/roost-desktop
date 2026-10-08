//! GNOME 51's desktop-level transitions, drawn at render time: the lock
//! curtain (`screenShield.js`), the screenshot flash (`screenshot.js`
//! `Flashspot`), the hot-corner ripples (`ripples.js`), the startup
//! reveal (`layout.js` `_startupAnimation`) and the monitor-change
//! transition (`layout.js` `ScreenTransition`).
//!
//! Each effect is a pure function of the time since it started, sampled
//! once per frame, and is dropped the moment it ends: an idle desktop
//! holds no effect, so the native backend's frame signature is unchanged
//! and nothing repaints. The wallpaper crossfade lives with the
//! wallpaper (`crate::wallpaper`), which owns both textures.
//!
//! The curtain never decides what may show: content stays hidden for as
//! long as the session is locked ([`crate::lock::content_visible`]).
//! While locking, the curtain only moves the lock surface and its
//! background over the plain lock clear; after authentication it lifts
//! a copy of the lock background off the session.

use std::time::Duration;

use tuna_shell_control::motion::{MotionLevel, MotionPolicy};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::Color32F;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};

use crate::runtime::View;

/// Clutter's `EASE_OUT_QUAD` over `t` in 0..1.
pub fn ease_out_quad(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Clutter's `EASE_IN_QUAD` over `t` in 0..1.
pub fn ease_in_quad(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t
}

fn ms(since: Duration) -> f64 {
    since.as_secs_f64() * 1000.0
}

/// GNOME's lowering of the lock screen (`Overview.ANIMATION_TIME`).
pub const CURTAIN_LOWER_MS: f64 = 250.0;
/// GNOME's `CURTAIN_SLIDE_TIME`: a full lift takes 300 ms, a partial one
/// proportionally less (the slide keeps one velocity).
pub const CURTAIN_LIFT_MS: f64 = 300.0;
/// How long a lock waits for the shell's lock surface before lowering
/// the bare background instead.
pub const CURTAIN_WAIT_MS: f64 = 500.0;

/// One eased leg of the curtain: `raised` goes `from` -> `to`.
#[derive(Debug, Clone, Copy)]
struct Leg {
    from: f64,
    to: f64,
    start: Duration,
    duration: f64,
    level: MotionLevel,
}

impl Leg {
    fn at(&self, now: Duration) -> f64 {
        if self.duration <= 0.0 {
            return self.to;
        }
        let p = ease_out_quad(ms(now.saturating_sub(self.start)) / self.duration);
        self.from + (self.to - self.from) * p
    }
    fn done(&self, now: Duration) -> bool {
        ms(now.saturating_sub(self.start)) >= self.duration
    }
}

/// Where the curtain is drawn this frame: raised by `lift` of the output
/// height (0 covers the screen) at opacity `alpha`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurtainFrame {
    pub lift: f64,
    pub alpha: f32,
}

impl CurtainFrame {
    /// The lift in physical pixels on an output `height` pixels tall.
    pub fn lift_px(&self, height: i32) -> i32 {
        (self.lift * f64::from(height)).round() as i32
    }
}

/// GNOME's lock curtain. `raised` is 1 when it is off the top of the
/// screen and 0 when it covers it. Locking waits for the lock surface
/// (at most [`CURTAIN_WAIT_MS`]) and lowers it over 250 ms; unlocking
/// lifts it at the curtain's one velocity, so a curtain caught part way
/// down lifts in proportionally less than 300 ms; locking again during a
/// lift settles it back down over 250 ms from where it is.
#[derive(Debug, Clone)]
pub struct Curtain {
    locked: bool,
    waiting: Option<Duration>,
    leg: Option<Leg>,
    raised: f64,
}

impl Default for Curtain {
    fn default() -> Self {
        Self {
            locked: false,
            waiting: None,
            leg: None,
            raised: 1.0,
        }
    }
}

impl Curtain {
    /// Follow the lock flag. `surface_ready` says the lock surface has
    /// mapped. Returns whether a slide began this frame.
    pub fn observe(
        &mut self,
        locked: bool,
        surface_ready: bool,
        now: Duration,
        policy: MotionPolicy,
    ) -> bool {
        self.raised = self.value(now);
        if self.leg.is_some_and(|leg| leg.done(now)) {
            self.leg = None;
        }
        let mut began = false;
        if locked && !self.locked {
            if self.raised >= 1.0 {
                self.waiting = Some(now);
            } else {
                // Caught mid-lift: settle back down from here.
                self.leg = Some(self.leg_to(0.0, CURTAIN_LOWER_MS, now, policy));
                began = true;
            }
        } else if !locked && self.locked {
            self.waiting = None;
            let remaining = 1.0 - self.raised;
            self.leg = Some(self.leg_to(1.0, CURTAIN_LIFT_MS * remaining, now, policy));
            began = remaining > 0.0;
        }
        self.locked = locked;
        if let Some(since) = self.waiting {
            if surface_ready || ms(now.saturating_sub(since)) >= CURTAIN_WAIT_MS {
                self.waiting = None;
                self.leg = Some(self.leg_to(0.0, CURTAIN_LOWER_MS, now, policy));
                began = true;
            }
        }
        began
    }

    /// A leg on GNOME's `adjustAnimationTime`: zero with animations
    /// off, stretched by the slow-down factor otherwise.
    fn leg_to(&self, to: f64, duration: f64, now: Duration, policy: MotionPolicy) -> Leg {
        Leg {
            from: self.raised,
            to,
            start: now,
            duration: policy.adjust_ms(duration),
            level: policy.level,
        }
    }

    fn value(&self, now: Duration) -> f64 {
        self.leg.map_or(self.raised, |leg| leg.at(now))
    }

    /// Whether a slide is under way at `now`.
    pub fn moving(&self, now: Duration) -> bool {
        self.leg
            .is_some_and(|leg| !leg.done(now) && leg.duration > 0.0)
    }

    /// The curtain at `now`, or `None` when it is fully lifted off an
    /// unlocked session (nothing to draw).
    pub fn frame(&self, now: Duration) -> Option<CurtainFrame> {
        let raised = self.value(now);
        if !self.locked && raised >= 1.0 {
            return None;
        }
        let fade = self
            .leg
            .is_some_and(|leg| leg.level == MotionLevel::FadeOnly);
        Some(if fade {
            CurtainFrame {
                lift: 0.0,
                alpha: (1.0 - raised) as f32,
            }
        } else {
            CurtainFrame {
                lift: raised,
                alpha: 1.0,
            }
        })
    }
}

/// GNOME's `FLASHSPOT_ANIMATION_OUT_TIME`.
pub const FLASH_MS: f64 = 500.0;

/// White over the captured area, easing out (`.flashspot`).
pub fn flash_alpha(elapsed_ms: f64) -> Option<f32> {
    (elapsed_ms < FLASH_MS).then(|| (1.0 - ease_out_quad(elapsed_ms / FLASH_MS)) as f32)
}

/// `ripples.js`: three rings, each `(delay, duration, start scale,
/// start opacity, final scale)`.
pub const RIPPLES: [(f64, f64, f64, f64, f64); 3] = [
    (0.0, 830.0, 0.25, 1.0, 1.5),
    (50.0, 1000.0, 0.0, 0.7, 1.25),
    (350.0, 1000.0, 0.0, 0.3, 1.0),
];
/// `.ripple-box`: 50px plus its 2px border.
pub const RIPPLE_SIZE: i32 = 52;

/// One ring's `(scale, opacity)` `elapsed_ms` after the corner fired,
/// or `None` once it has finished. A ring shows at its start values
/// through its delay (GNOME sets them before easing).
pub fn ripple_ring(ring: usize, elapsed_ms: f64) -> Option<(f64, f64)> {
    let (delay, duration, start_scale, start_opacity, final_scale) = *RIPPLES.get(ring)?;
    if elapsed_ms >= delay + duration {
        return None;
    }
    let p = ((elapsed_ms - delay) / duration).clamp(0.0, 1.0);
    let opacity = start_opacity.sqrt() * (1.0 - ease_in_quad(p));
    let scale = start_scale + (final_scale - start_scale) * ease_out_quad(p);
    Some((scale, opacity))
}

/// The longest ring's end.
fn ripples_end() -> f64 {
    RIPPLES
        .iter()
        .map(|(delay, duration, ..)| delay + duration)
        .fold(0.0, f64::max)
}

/// `.ripple-box` as premultiplied ARGB8888: white at 20% in a quarter
/// disc whose corner sits at the top-left (`mirrored`: top-right), with
/// its 2px glow. `size` square.
pub fn ripple_pixels(size: u32, mirrored: bool) -> Vec<u8> {
    let r = f64::from(size);
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let cx = if mirrored { size - 1 - x } else { x };
            let d = (f64::from(cx) + 0.5).hypot(f64::from(y) + 0.5);
            // Solid to the radius less the glow, then the glow fades out.
            let coverage = ((r - d) / 2.0).clamp(0.0, 1.0);
            let a = (0.2 * coverage * 255.0).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&[a, a, a, a]);
        }
    }
    out
}

/// The hot corner the pointer at `pos` pressed: the top-left of the
/// monitor under it, or its top-right in a right-to-left locale (then
/// `true`, the ripples mirrored).
pub fn hot_corner(
    outputs: &[(Rectangle<i32, Logical>, bool)],
    pos: Point<f64, Logical>,
    right_to_left: bool,
) -> Option<(Point<i32, Logical>, bool)> {
    let inside = |r: &Rectangle<i32, Logical>| {
        pos.x >= f64::from(r.loc.x)
            && pos.y >= f64::from(r.loc.y)
            && pos.x <= f64::from(r.loc.x + r.size.w)
            && pos.y <= f64::from(r.loc.y + r.size.h)
    };
    let (rect, _) = outputs
        .iter()
        .find(|(r, _)| inside(r))
        .or_else(|| outputs.iter().find(|(_, primary)| *primary))?;
    let x = if right_to_left {
        rect.loc.x + rect.size.w
    } else {
        rect.loc.x
    };
    Some(((x, rect.loc.y).into(), right_to_left))
}

/// `layout.js` `STARTUP_ANIMATION_TIME`.
pub const STARTUP_MS: f64 = 500.0;
/// The longest the startup cover waits for the shell's first surface.
pub const STARTUP_HOLD_MS: f64 = 3000.0;
/// `ScreenTransition`: the old picture holds 250 ms, then fades 500 ms.
pub const SCREEN_DELAY_MS: f64 = 250.0;
pub const SCREEN_FADE_MS: f64 = 500.0;

/// Opacity of the cover `elapsed_ms` after the shell was ready.
pub fn startup_alpha(elapsed_ms: f64) -> Option<f32> {
    (elapsed_ms < STARTUP_MS).then(|| (1.0 - ease_out_quad(elapsed_ms / STARTUP_MS)) as f32)
}

/// Opacity of the old screen's picture `elapsed_ms` after the change.
pub fn screen_alpha(elapsed_ms: f64) -> Option<f32> {
    let fade = elapsed_ms - SCREEN_DELAY_MS;
    (fade < SCREEN_FADE_MS).then(|| (1.0 - ease_out_quad(fade / SCREEN_FADE_MS)) as f32)
}

/// What one family did, for the proofs: transitions begun, frames drawn
/// part way through one, whether one is running, and its current value
/// (the curtain's lift, an overlay's opacity), sampled on the animation
/// clock so a proof can check exact delays.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Counter {
    pub started: u64,
    pub animated_frames: u64,
    pub active: bool,
    pub value: Option<f64>,
}

impl Counter {
    fn sample(&mut self, active: bool, value: Option<f64>) {
        self.active = active;
        self.value = value;
        if active {
            self.animated_frames += 1;
        }
    }
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "started": self.started,
            "animated_frames": self.animated_frames,
            "active": self.active,
            "value": self.value,
        })
    }
}

/// The old screen's picture per output (`None`: the nested window).
struct Snapshot {
    output: Option<String>,
    buffer: MemoryRenderBuffer,
}

/// Effects drawn over one output, above the scene and below the idle
/// shield. Textures are physical-sized; both lists are topmost first.
#[derive(Debug, Default)]
pub struct Overlay {
    pub textures: Vec<MemoryRenderBufferRenderElement<GlesRenderer>>,
    pub solids: Vec<SolidColorRenderElement>,
}

/// Every desktop-level transition and its clock.
pub struct Transitions {
    policy: MotionPolicy,
    now: Duration,
    born: Duration,
    curtain: Curtain,
    flash: Option<(Rectangle<i32, Logical>, Duration)>,
    ripples: Option<(Point<i32, Logical>, bool, Duration)>,
    revealed: Option<Duration>,
    screen: Option<(Vec<Snapshot>, Duration)>,
    ripple_textures: [Option<MemoryRenderBuffer>; 2],
    ids: [Id; 2],
    stats: [Counter; 5],
}

const CURTAIN: usize = 0;
const FLASH: usize = 1;
const RIPPLE: usize = 2;
const STARTUP: usize = 3;
const SCREEN: usize = 4;

impl std::fmt::Debug for Transitions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transitions")
            .field("policy", &self.policy)
            .field("curtain", &self.curtain)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Transitions {
    pub fn new(now: Duration) -> Self {
        Self {
            policy: MotionPolicy::default(),
            now,
            born: now,
            curtain: Curtain::default(),
            flash: None,
            ripples: None,
            revealed: None,
            screen: None,
            ripple_textures: [None, None],
            ids: [Id::new(), Id::new()],
            stats: [Counter::default(); 5],
        }
    }

    /// Apply GNOME's motion policy. What it no longer allows ends.
    pub fn set_policy(&mut self, policy: MotionPolicy) {
        self.policy = policy;
        if !policy.allows_fades() {
            self.flash = None;
            self.screen = None;
        }
        if !policy.allows_motion() {
            self.ripples = None;
        }
    }

    /// Milliseconds since `since` on the effects' own timeline: the
    /// slow-down factor stretches every delay and duration alike.
    fn elapsed(&self, since: Duration) -> f64 {
        ms(self.now.saturating_sub(since)) / self.policy.slowdown()
    }

    /// Advance to `now`, once per frame, before building any output's
    /// overlay. Finished effects are dropped here so an idle desktop
    /// carries none.
    pub fn advance(&mut self, now: Duration, locked: bool, lock_ready: bool, shell_ready: bool) {
        self.now = now;
        if self.curtain.observe(locked, lock_ready, now, self.policy) {
            self.stats[CURTAIN].started += 1;
        }
        let moving = self.curtain.moving(now);
        let lift = self.curtain.frame(now).map(|c| c.lift);
        self.stats[CURTAIN].sample(moving, lift);
        if self.revealed.is_none()
            && (shell_ready || locked || ms(now.saturating_sub(self.born)) >= STARTUP_HOLD_MS)
        {
            self.revealed = Some(now);
            self.stats[STARTUP].started += 1;
        }
        let cover = self.startup_cover();
        let startup = cover.is_some_and(|a| a < 1.0 && a > 0.0);
        self.stats[STARTUP].sample(startup, cover.map(f64::from));
        if self
            .flash
            .is_some_and(|(_, at)| flash_alpha(self.elapsed(at)).is_none())
        {
            self.flash = None;
        }
        let flash = self
            .flash
            .and_then(|(_, at)| flash_alpha(self.elapsed(at)))
            .map(f64::from);
        self.stats[FLASH].sample(self.flash.is_some(), flash);
        if self
            .ripples
            .is_some_and(|(_, _, at)| self.elapsed(at) >= ripples_end())
        {
            self.ripples = None;
        }
        let ring = self
            .ripples
            .and_then(|(_, _, at)| ripple_ring(0, self.elapsed(at)))
            .map(|(_, opacity)| opacity);
        self.stats[RIPPLE].sample(self.ripples.is_some(), ring);
        // The old screen is session content: never over a lock.
        if locked
            || self
                .screen
                .as_ref()
                .is_some_and(|(_, at)| screen_alpha(self.elapsed(*at)).is_none())
        {
            self.screen = None;
        }
        let screen = self
            .screen
            .as_ref()
            .and_then(|(_, at)| screen_alpha(self.elapsed(*at)))
            .map(f64::from);
        self.stats[SCREEN].sample(self.screen.is_some(), screen);
    }

    /// The lock curtain this frame (see [`Curtain::frame`]).
    pub fn curtain(&self) -> Option<CurtainFrame> {
        self.curtain.frame(self.now)
    }

    /// Flash `area` (global logical) white, as GNOME does when a
    /// screenshot asks for it. Nothing when animations are off.
    pub fn flash(&mut self, area: Rectangle<i32, Logical>) {
        if self.policy.allows_fades() && !area.is_empty() {
            self.flash = Some((area, self.now));
            self.stats[FLASH].started += 1;
        }
    }

    /// Ripple out of the hot corner at `corner` (global logical);
    /// `mirrored` for a right-to-left top-right corner. GNOME plays
    /// them only while the overview animates; they are motion, so never
    /// under reduced motion or with animations off.
    pub fn ripple(&mut self, corner: Point<i32, Logical>, mirrored: bool) {
        if self.policy.allows_motion() {
            self.ripples = Some((corner, mirrored, self.now));
            self.stats[RIPPLE].started += 1;
        }
    }

    /// Whether a monitor change should capture the old screen first.
    pub fn wants_screen_snapshot(&self) -> bool {
        self.policy.allows_fades()
    }

    /// The monitors changed: fade out the old screen, one XRGB8888
    /// picture per output, over the new layout.
    pub fn screen_changed(&mut self, pictures: Vec<(Option<String>, i32, i32, Vec<u8>)>) {
        if pictures.is_empty() || !self.policy.allows_fades() {
            return;
        }
        let snapshots = pictures
            .into_iter()
            .map(|(output, w, h, pixels)| Snapshot {
                output,
                buffer: MemoryRenderBuffer::from_slice(
                    &pixels,
                    Fourcc::Xrgb8888,
                    (w, h),
                    1,
                    Transform::Normal,
                    None,
                ),
            })
            .collect();
        self.screen = Some((snapshots, self.now));
        self.stats[SCREEN].started += 1;
    }

    fn startup_cover(&self) -> Option<f32> {
        match self.revealed {
            None => Some(1.0),
            Some(_) if !self.policy.allows_fades() => None,
            Some(at) => startup_alpha(self.elapsed(at)),
        }
    }

    /// The overlay for one output of `size` physical pixels seen through
    /// `view` (`output`: its connector, `None` nested).
    #[allow(clippy::too_many_arguments)]
    pub fn overlay(
        &mut self,
        renderer: &mut GlesRenderer,
        wallpaper: &mut crate::wallpaper::Wallpaper,
        view: View,
        size: Size<i32, Physical>,
        geometry: tuna_wallpaper::background::Geometry,
        output: Option<&str>,
        locked: bool,
    ) -> Overlay {
        let mut overlay = Overlay::default();
        let whole = Rectangle::from_size(size);
        if let Some(alpha) = self.startup_cover() {
            overlay.solids.push(solid(
                &self.ids[0],
                whole,
                Color32F::new(0.0, 0.0, 0.0, alpha),
            ));
        }
        if let Some((area, at)) = self.flash {
            if let Some(alpha) = flash_alpha(self.elapsed(at)) {
                let loc = view.physical(f64::from(area.loc.x), f64::from(area.loc.y));
                let end = view.physical(
                    f64::from(area.loc.x + area.size.w),
                    f64::from(area.loc.y + area.size.h),
                );
                let rect = Rectangle::new(loc, (end.x - loc.x, end.y - loc.y).into());
                if let Some(rect) = rect.intersection(whole) {
                    overlay.solids.push(solid(
                        &self.ids[1],
                        rect,
                        Color32F::new(alpha, alpha, alpha, alpha),
                    ));
                }
            }
        }
        if let Some((snapshots, at)) = self.screen.as_ref().filter(|_| !locked) {
            let alpha = screen_alpha(self.elapsed(*at));
            if let (Some(alpha), Some(snapshot)) = (
                alpha,
                snapshots.iter().find(|s| s.output.as_deref() == output),
            ) {
                overlay.textures.extend(
                    MemoryRenderBufferRenderElement::from_buffer(
                        renderer,
                        Point::<f64, Physical>::from((0.0, 0.0)),
                        &snapshot.buffer,
                        Some(alpha),
                        None,
                        Some((size.w, size.h).into()),
                        Kind::Unspecified,
                    )
                    .ok(),
                );
            }
        }
        if let Some((corner, mirrored, at)) = self.ripples {
            let texture = self.ripple_textures[usize::from(mirrored)]
                .get_or_insert_with(|| {
                    MemoryRenderBuffer::from_slice(
                        &ripple_pixels(RIPPLE_SIZE as u32, mirrored),
                        Fourcc::Argb8888,
                        (RIPPLE_SIZE, RIPPLE_SIZE),
                        1,
                        Transform::Normal,
                        None,
                    )
                })
                .clone();
            let origin = view.physical(f64::from(corner.x), f64::from(corner.y));
            // Later rings draw above earlier ones: topmost first.
            for ring in (0..RIPPLES.len()).rev() {
                let Some((scale, opacity)) = ripple_ring(ring, self.elapsed(at)) else {
                    continue;
                };
                let side = (f64::from(RIPPLE_SIZE) * scale * view.scale).round() as i32;
                if side <= 0 || opacity <= 0.0 {
                    continue;
                }
                let x = if mirrored { origin.x - side } else { origin.x };
                overlay.textures.extend(
                    MemoryRenderBufferRenderElement::from_buffer(
                        renderer,
                        Point::<f64, Physical>::from((f64::from(x), f64::from(origin.y))),
                        &texture,
                        Some(opacity as f32),
                        None,
                        Some((side, side).into()),
                        Kind::Unspecified,
                    )
                    .ok(),
                );
            }
        }
        // After authentication: the lock background lifting off the
        // session (while locked the curtain moves the lock layer itself).
        if let Some(curtain) = self.curtain().filter(|_| !locked) {
            overlay.textures.extend(wallpaper.lock_element(
                renderer,
                size.w,
                size.h,
                geometry,
                curtain.lift_px(size.h),
                curtain.alpha,
            ));
        }
        overlay
    }

    /// Per-family counters for the compositor state document.
    pub fn diagnostics(&self) -> serde_json::Value {
        let mut curtain = self.stats[CURTAIN].json();
        curtain["alpha"] = serde_json::json!(self.curtain().map(|c| c.alpha));
        serde_json::json!({
            "lock_curtain": curtain,
            "screenshot_flash": self.stats[FLASH].json(),
            "hot_corner_ripples": self.stats[RIPPLE].json(),
            "startup": self.stats[STARTUP].json(),
            "monitor_change": self.stats[SCREEN].json(),
        })
    }
}

/// A translucent solid whose commit tracks its colour, so the damage
/// tracker repaints it as it fades.
fn solid(id: &Id, rect: Rectangle<i32, Physical>, color: Color32F) -> SolidColorRenderElement {
    let commit = (color.a() * 1000.0).round() as usize;
    SolidColorRenderElement::new(id.clone(), rect, commit, color, Kind::Unspecified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(level: MotionLevel) -> MotionPolicy {
        MotionPolicy::new(level, 1.0)
    }

    fn at(start: Duration, ms: u64) -> Duration {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn curves_match_clutter() {
        assert_eq!(ease_out_quad(0.5), 0.75);
        assert_eq!(ease_in_quad(0.5), 0.25);
        assert_eq!(ease_out_quad(2.0), 1.0);
        assert_eq!(ease_in_quad(-1.0), 0.0);
    }

    #[test]
    fn locking_waits_for_the_surface_then_lowers_over_250_ms() {
        let t = Duration::ZERO;
        let mut curtain = Curtain::default();
        assert!(
            curtain.frame(t).is_none(),
            "unlocked and lifted: no curtain"
        );
        assert!(!curtain.observe(true, false, t, policy(MotionLevel::Full)));
        assert_eq!(
            curtain.frame(t).unwrap().lift,
            1.0,
            "held up for the surface"
        );
        assert!(curtain.observe(true, true, at(t, 40), policy(MotionLevel::Full)));
        let mid = curtain.frame(at(t, 165)).unwrap();
        assert!((mid.lift - 0.25).abs() < 1e-9, "ease-out-quad half way");
        assert!(curtain.moving(at(t, 165)));
        assert!(!curtain.observe(true, true, at(t, 290), policy(MotionLevel::Full)));
        assert_eq!(curtain.frame(at(t, 290)).unwrap().lift, 0.0);
        assert!(!curtain.moving(at(t, 290)));
    }

    #[test]
    fn a_missing_lock_surface_lowers_the_bare_background_after_the_wait() {
        let t = Duration::ZERO;
        let mut curtain = Curtain::default();
        curtain.observe(true, false, t, policy(MotionLevel::Full));
        assert!(!curtain.observe(true, false, at(t, 499), policy(MotionLevel::Full)));
        assert!(curtain.observe(true, false, at(t, 500), policy(MotionLevel::Full)));
    }

    #[test]
    fn unlocking_lifts_at_one_velocity_and_relocking_settles_back() {
        let t = Duration::ZERO;
        let mut curtain = Curtain::default();
        curtain.observe(true, true, t, policy(MotionLevel::Full));
        curtain.observe(true, true, at(t, 250), policy(MotionLevel::Full));
        // A full lift: 300 ms.
        assert!(curtain.observe(false, true, at(t, 1000), policy(MotionLevel::Full)));
        assert!((curtain.frame(at(t, 1150)).unwrap().lift - 0.75).abs() < 1e-9);
        curtain.observe(false, false, at(t, 1300), policy(MotionLevel::Full));
        assert!(
            curtain.frame(at(t, 1300)).is_none(),
            "lifted off: nothing drawn"
        );
        // Unlocked part way down (raised 0.5): the lift takes 150 ms.
        let mut curtain = Curtain::default();
        curtain.observe(true, true, t, policy(MotionLevel::Full));
        let raised = curtain.frame(at(t, 73)).unwrap().lift;
        curtain.observe(false, true, at(t, 73), policy(MotionLevel::Full));
        let leg = curtain.leg.unwrap();
        assert!((leg.duration - CURTAIN_LIFT_MS * (1.0 - raised)).abs() < 1e-9);
        // Locked again mid-lift: back down over 250 ms from where it is.
        let here = curtain.frame(at(t, 100)).unwrap().lift;
        assert!(curtain.observe(true, true, at(t, 100), policy(MotionLevel::Full)));
        assert!((curtain.frame(at(t, 100)).unwrap().lift - here).abs() < 1e-9);
        curtain.observe(true, true, at(t, 350), policy(MotionLevel::Full));
        assert_eq!(curtain.frame(at(t, 350)).unwrap().lift, 0.0);
    }

    #[test]
    fn the_curtain_is_instant_with_animations_off_and_fades_under_reduced_motion() {
        let t = Duration::ZERO;
        let mut curtain = Curtain::default();
        curtain.observe(true, true, t, policy(MotionLevel::Off));
        assert_eq!(curtain.frame(t).unwrap().lift, 0.0);
        assert!(!curtain.moving(t));
        curtain.observe(false, true, at(t, 1), policy(MotionLevel::Off));
        assert!(curtain.frame(at(t, 1)).is_none());

        let mut curtain = Curtain::default();
        curtain.observe(true, true, t, policy(MotionLevel::FadeOnly));
        let mid = curtain.frame(at(t, 125)).unwrap();
        assert_eq!(mid.lift, 0.0, "reduced motion never slides");
        assert!((mid.alpha - 0.75).abs() < 1e-6);
    }

    #[test]
    fn flash_eases_out_over_500_ms() {
        assert_eq!(flash_alpha(0.0), Some(1.0));
        assert_eq!(flash_alpha(250.0), Some(0.25));
        assert_eq!(flash_alpha(500.0), None);
    }

    #[test]
    fn ripples_follow_gnome_rings() {
        // Ring one starts at a quarter size, full opacity.
        assert_eq!(ripple_ring(0, 0.0), Some((0.25, 1.0)));
        // Ring three waits 350 ms at its start values.
        assert_eq!(ripple_ring(2, 100.0), Some((0.0, 0.3f64.sqrt())));
        let (scale, opacity) = ripple_ring(1, 550.0).unwrap();
        assert!((scale - (0.0 + 1.25 * 0.75)).abs() < 1e-9);
        assert!((opacity - 0.7f64.sqrt() * 0.75).abs() < 1e-9);
        assert_eq!(ripple_ring(0, 830.0), None);
        assert_eq!(ripples_end(), 1350.0);
    }

    #[test]
    fn ripple_box_is_a_translucent_quarter_disc() {
        let px = ripple_pixels(52, false);
        let alpha = |x: u32, y: u32| px[((y * 52 + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 51, "20% white at the corner");
        assert_eq!(alpha(51, 51), 0, "outside the curve");
        let mirrored = ripple_pixels(52, true);
        assert_eq!(mirrored[(51 * 4 + 3) as usize], 51);
    }

    #[test]
    fn ripples_start_at_the_pressed_monitors_corner() {
        let outputs = [
            (Rectangle::new((0, 0).into(), (1280, 800).into()), true),
            (Rectangle::new((1280, 0).into(), (1920, 1080).into()), false),
        ];
        let corner = |x, y, rtl| hot_corner(&outputs, Point::from((x, y)), rtl);
        assert_eq!(corner(0.0, 0.0, false), Some(((0, 0).into(), false)));
        assert_eq!(corner(1290.0, 0.0, false), Some(((1280, 0).into(), false)));
        assert_eq!(corner(3199.0, 0.0, true), Some(((3200, 0).into(), true)));
        assert_eq!(hot_corner(&[], Point::from((0.0, 0.0)), false), None);
    }

    #[test]
    fn startup_and_monitor_change_fade_on_gnome_timing() {
        assert_eq!(startup_alpha(0.0), Some(1.0));
        assert_eq!(startup_alpha(250.0), Some(0.25));
        assert_eq!(startup_alpha(500.0), None);
        assert_eq!(screen_alpha(200.0), Some(1.0), "held for the delay");
        assert_eq!(screen_alpha(500.0), Some(0.25));
        assert_eq!(screen_alpha(750.0), None);
    }

    #[test]
    fn effects_end_and_animations_off_starts_none() {
        let t = Duration::ZERO;
        let mut fx = Transitions::new(t);
        fx.advance(t, false, false, true);
        fx.flash(Rectangle::new((0, 0).into(), (10, 10).into()));
        fx.ripple((0, 0).into(), false);
        fx.advance(at(t, 100), false, false, true);
        assert!(fx.stats[FLASH].active && fx.stats[RIPPLE].active);
        fx.advance(at(t, 2000), false, false, true);
        assert!(
            fx.flash.is_none() && fx.ripples.is_none(),
            "idle holds no effect"
        );
        assert!(fx.startup_cover().is_none());
        fx.set_policy(policy(MotionLevel::Off));
        fx.flash(Rectangle::new((0, 0).into(), (10, 10).into()));
        fx.ripple((0, 0).into(), false);
        assert!(!fx.wants_screen_snapshot());
        assert!(fx.flash.is_none() && fx.ripples.is_none());
        assert_eq!(fx.stats[FLASH].started, 1);
    }

    #[test]
    fn reduced_motion_keeps_fades_and_the_slowdown_stretches_them() {
        let t = Duration::ZERO;
        let mut fx = Transitions::new(t);
        fx.set_policy(policy(MotionLevel::FadeOnly));
        fx.advance(t, false, false, true);
        fx.flash(Rectangle::new((0, 0).into(), (10, 10).into()));
        fx.ripple((0, 0).into(), false);
        assert!(fx.flash.is_some(), "the flash is a fade");
        assert!(fx.ripples.is_none(), "ripples are motion");
        let mut fx = Transitions::new(t);
        fx.set_policy(MotionPolicy::new(MotionLevel::Full, 2.0));
        fx.advance(t, false, false, true);
        fx.flash(Rectangle::new((0, 0).into(), (10, 10).into()));
        fx.advance(at(t, 999), false, false, true);
        assert!(fx.flash.is_some(), "500 ms at factor 2 lasts 1000 ms");
        fx.advance(at(t, 1000), false, false, true);
        assert!(fx.flash.is_none());
        let mut curtain = Curtain::default();
        curtain.observe(true, true, t, MotionPolicy::new(MotionLevel::Full, 2.0));
        assert!((curtain.leg.unwrap().duration - 500.0).abs() < 1e-9);
    }

    #[test]
    fn startup_holds_until_the_shell_is_ready_or_the_cap() {
        let t = Duration::ZERO;
        let mut fx = Transitions::new(t);
        fx.advance(at(t, 1000), false, false, false);
        assert_eq!(fx.startup_cover(), Some(1.0));
        fx.advance(at(t, 1200), false, false, true);
        assert_eq!(fx.startup_cover(), Some(1.0));
        fx.advance(at(t, 1450), false, false, true);
        assert_eq!(fx.startup_cover(), Some(0.25));
        let mut fx = Transitions::new(t);
        fx.advance(at(t, 3000), false, false, false);
        assert_eq!(fx.stats[STARTUP].started, 1, "never black for good");
        let mut fx = Transitions::new(t);
        fx.set_policy(policy(MotionLevel::Off));
        fx.advance(at(t, 10), false, false, true);
        assert!(fx.startup_cover().is_none(), "instant with animations off");
    }

    #[test]
    fn the_old_screen_never_shows_over_a_lock() {
        let t = Duration::ZERO;
        let mut fx = Transitions::new(t);
        fx.advance(t, false, false, true);
        fx.screen_changed(vec![(None, 2, 2, vec![0; 16])]);
        assert!(fx.screen.is_some());
        fx.advance(at(t, 100), true, false, true);
        assert!(fx.screen.is_none());
    }
}
