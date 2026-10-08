//! GNOME 51's workspace switch animation (`workspaceAnimation.js`,
//! `swipeTracker.js`).
//!
//! Workspaces sit side by side, one output width plus 100px apart. A
//! switch moves the view along that strip: 250 ms `EASE_OUT_CUBIC` from
//! wherever it is drawn now, so a second key press mid-flight carries on
//! from the current offset instead of jumping back. A three-finger swipe
//! drags the view with the fingers; on release GNOME projects the
//! finger velocity onto the strip, picks the workspace it lands on and
//! finishes in 100-400 ms depending on how fast the fingers were moving.
//!
//! The model switches workspace at once (focus and input follow without
//! waiting); only the drawn position of each workspace's windows trails
//! it. Nothing is reconfigured: the offset is applied at render time.
//! Under a fade-only motion policy the workspaces stay in place and
//! crossfade over the same timeline; with motion off nothing animates.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// `WINDOW_ANIMATION_TIME`: the keyboard switch.
pub const SWITCH_MS: f64 = 250.0;
/// `WORKSPACE_SPACING`: the gap between neighbouring workspaces.
pub const WORKSPACE_SPACING: i32 = 100;
/// `TOUCHPAD_BASE_WIDTH`: finger travel that moves one whole workspace.
pub const TOUCHPAD_BASE_WIDTH: f64 = 400.0;
/// `EVENT_HISTORY_THRESHOLD_MS`: the window the release velocity is
/// measured over.
const HISTORY_MS: u32 = 150;
const MIN_SWIPE_MS: f64 = 100.0;
const MAX_SWIPE_MS: f64 = 400.0;
const VELOCITY_THRESHOLD: f64 = 0.6;
const DECELERATION: f64 = 0.997;
const VELOCITY_CURVE_THRESHOLD: f64 = 2.0;
const DECELERATION_PARABOLA_MULTIPLIER: f64 = 0.35;
/// The derivative of ease-out-cubic at 0: a release keeps its speed.
const DURATION_MULTIPLIER: f64 = 3.0;
const ANIMATION_BASE_VELOCITY: f64 = 0.002;
/// Finished motions kept for the state snapshot.
const HISTORY_LEN: usize = 8;

/// Clutter's `EASE_OUT_CUBIC`.
pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0) - 1.0;
    t * t * t + 1.0
}

/// How the switch is shown. Today the compositor only knows GNOME's
/// on/off animation preference; a fade-only policy (reduced motion that
/// still allows opacity) maps to [`Motion::Fade`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Motion {
    Slide,
    Fade,
    Off,
}

impl Motion {
    fn name(self) -> &'static str {
        match self {
            Motion::Slide => "slide",
            Motion::Fade => "fade",
            Motion::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionPolicy {
    pub motion: Motion,
    /// Duration multiplier (1.0 is GNOME's speed).
    pub slowdown: f64,
}

impl MotionPolicy {
    /// The policy for GNOME's effective `enable-animations`.
    pub fn from_enabled(enabled: bool) -> Self {
        Self {
            motion: if enabled { Motion::Slide } else { Motion::Off },
            slowdown: 1.0,
        }
    }

    fn scaled(self, ms: f64) -> Duration {
        Duration::from_secs_f64((ms * self.slowdown.max(0.0)) / 1000.0)
    }
}

/// Recent finger deltas (`EventHistory`): the release velocity is their
/// sum over the last 150 ms, in px/ms.
#[derive(Debug, Default, Clone)]
pub struct SwipeHistory {
    samples: VecDeque<(u32, f64)>,
}

impl SwipeHistory {
    fn trim(&mut self, time: u32) {
        let threshold = time.saturating_sub(HISTORY_MS);
        while self.samples.front().is_some_and(|(t, _)| *t < threshold) {
            self.samples.pop_front();
        }
    }

    pub fn append(&mut self, time: u32, delta: f64) {
        self.trim(time);
        self.samples.push_back((time, delta));
    }

    /// Velocity at `time` (px/ms); zero without two samples in range.
    pub fn velocity(&mut self, time: u32) -> f64 {
        self.trim(time);
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return 0.0;
        };
        if self.samples.len() < 2 || first.0 == last.0 {
            return 0.0;
        }
        let period = f64::from(last.0 - first.0);
        self.samples.iter().skip(1).map(|(_, d)| d).sum::<f64>() / period
    }
}

/// Where a released swipe settles and how long it takes to get there
/// (`_getEndProgress` and `_endGesture` for a touchpad). Positions are
/// workspace slots; `velocity` is px/ms along the strip.
pub fn swipe_release(
    progress: f64,
    initial: f64,
    slots: usize,
    velocity: f64,
    cancelled: bool,
) -> (f64, f64) {
    let last = slots.saturating_sub(1) as f64;
    let closest = |p: f64| p.round().clamp(0.0, last);
    let (low, high) = ((initial - 1.0).max(0.0), (initial + 1.0).min(last));
    let end = if cancelled {
        initial
    } else if velocity.abs() < VELOCITY_THRESHOLD {
        closest(progress)
    } else {
        let slope = DECELERATION / (1.0 - DECELERATION) / 1000.0;
        let speed = velocity.abs();
        let travel = if speed > VELOCITY_CURVE_THRESHOLD {
            let c = slope / 2.0 / DECELERATION_PARABOLA_MULTIPLIER;
            let x = speed - VELOCITY_CURVE_THRESHOLD + c;
            slope * VELOCITY_CURVE_THRESHOLD + DECELERATION_PARABOLA_MULTIPLIER * x * x
                - DECELERATION_PARABOLA_MULTIPLIER * c * c
        } else {
            speed * slope
        };
        let pos = (progress + travel * velocity.signum()).clamp(low, high);
        // `_findPointForProjection`: a projection that stays on the
        // starting workspace still moves one in the swipe's direction.
        let (prev, next) = (pos.floor(), pos.ceil());
        if velocity > 0.0 && prev == initial {
            next
        } else if velocity < 0.0 && next == initial {
            prev
        } else {
            closest(pos)
        }
    };
    let mut speed = velocity / TOUCHPAD_BASE_WIDTH;
    if (end - progress) * speed <= 0.0 {
        speed = ANIMATION_BASE_VELOCITY;
    }
    let points = (progress - end).abs().ceil().max(1.0);
    let max = MAX_SWIPE_MS * (1.0 + points).log2();
    let mut duration = ((progress - end) / speed * DURATION_MULTIPLIER).abs();
    if duration > 0.0 {
        duration = duration.clamp(MIN_SWIPE_MS, max);
    }
    (end, duration)
}

/// One switch, for the state snapshot and the proofs.
#[derive(Debug, Clone, PartialEq)]
pub struct SlideRecord {
    pub from: u32,
    pub to: u32,
    pub motion: Motion,
    pub gesture: bool,
    /// The last planned duration (after any retarget).
    pub duration_ms: f64,
    /// Start to settle, retargets included.
    pub elapsed_ms: f64,
    pub frames: u32,
    pub retargets: u32,
    /// Whether any drawn frame moved against the direction of travel.
    pub reversed: bool,
    /// The release velocity of a swipe (px/ms).
    pub velocity: f64,
    started: Instant,
}

impl SlideRecord {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "from": self.from,
            "to": self.to,
            "motion": self.motion.name(),
            "gesture": self.gesture,
            "duration_ms": self.duration_ms.round(),
            "elapsed_ms": self.elapsed_ms.round(),
            "frames": self.frames,
            "retargets": self.retargets,
            "reversed": self.reversed,
            "velocity": self.velocity,
        })
    }
}

#[derive(Debug, Clone)]
enum Phase {
    Idle,
    Easing {
        from: f64,
        to: f64,
        start: Instant,
        duration: Duration,
    },
    Following {
        progress: f64,
        initial: f64,
        history: SwipeHistory,
    },
}

/// The workspace strip's drawn position and its motion.
#[derive(Debug, Clone)]
pub struct WorkspaceSlide {
    /// Workspace ids left to right while a motion runs.
    strip: Vec<u32>,
    /// Pixels from one workspace to the next.
    distance: f64,
    policy: MotionPolicy,
    phase: Phase,
    /// The active workspace when last observed.
    seen: Option<u32>,
    last_value: Option<f64>,
    /// The time the current frame draws at.
    frame: Option<Instant>,
    record: Option<SlideRecord>,
    finished: VecDeque<SlideRecord>,
}

impl Default for WorkspaceSlide {
    fn default() -> Self {
        Self {
            strip: Vec::new(),
            distance: 0.0,
            policy: MotionPolicy::from_enabled(true),
            phase: Phase::Idle,
            seen: None,
            last_value: None,
            frame: None,
            record: None,
            finished: VecDeque::new(),
        }
    }
}

/// How one workspace's windows are drawn this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub dx: i32,
    pub alpha: f32,
}

impl WorkspaceSlide {
    /// Whether a motion is drawn (motion off draws nothing).
    pub fn running(&self) -> bool {
        self.policy.motion != Motion::Off && !matches!(self.phase, Phase::Idle)
    }

    /// Whether fingers hold the strip.
    pub fn following(&self) -> bool {
        matches!(self.phase, Phase::Following { .. })
    }

    /// The drawn strip position in slots, while anything moves.
    pub fn value(&self, now: Instant) -> Option<f64> {
        match &self.phase {
            Phase::Idle => None,
            Phase::Following { progress, .. } => Some(*progress),
            Phase::Easing {
                from,
                to,
                start,
                duration,
            } => {
                let t = if duration.is_zero() {
                    1.0
                } else {
                    now.saturating_duration_since(*start).as_secs_f64() / duration.as_secs_f64()
                };
                Some(from + (to - from) * ease_out_cubic(t))
            }
        }
    }

    fn slot(&self, workspace: u32) -> Option<f64> {
        self.strip
            .iter()
            .position(|w| *w == workspace)
            .map(|i| i as f64)
    }

    /// Where `workspace`'s windows draw this frame, or `None` when they
    /// are out of view. Only meaningful while [`running`](Self::running).
    pub fn placement(&self, workspace: u32, now: Instant) -> Option<Placement> {
        let d = self.slot(workspace)? - self.value(now)?;
        if d.abs() >= 1.0 {
            return None;
        }
        Some(match self.policy.motion {
            Motion::Fade => Placement {
                dx: 0,
                alpha: (1.0 - d.abs()) as f32,
            },
            _ => Placement {
                dx: (d * self.distance).round() as i32,
                alpha: 1.0,
            },
        })
    }

    /// [`placement`](Self::placement) at the last stepped frame.
    pub fn frame_placement(&self, workspace: u32) -> Option<Placement> {
        self.placement(workspace, self.frame?)
    }

    /// The strip for a motion: the known workspaces, `extra` and what is
    /// already laid out, in id order (ids are GNOME's order). Positions
    /// already in flight move with their workspaces.
    fn merge_strip(&mut self, workspaces: &[u32], extra: &[u32]) {
        let old = std::mem::take(&mut self.strip);
        let mut strip: Vec<u32> = old.iter().chain(workspaces).chain(extra).copied().collect();
        strip.sort_unstable();
        strip.dedup();
        let remap = |v: &mut f64| {
            let base = v.floor().max(0.0) as usize;
            if let Some(i) = old
                .get(base)
                .and_then(|id| strip.iter().position(|w| w == id))
            {
                *v += i as f64 - base as f64;
            }
        };
        match &mut self.phase {
            Phase::Easing { from, to, .. } => {
                remap(from);
                remap(to);
            }
            Phase::Following {
                progress, initial, ..
            } => {
                remap(progress);
                remap(initial);
            }
            Phase::Idle => {}
        }
        if let Some(v) = self.last_value.as_mut() {
            remap(v);
        }
        self.strip = strip;
    }

    fn archive(&mut self, now: Instant) {
        if let Some(mut record) = self.record.take() {
            record.elapsed_ms =
                now.saturating_duration_since(record.started).as_secs_f64() * 1000.0;
            if self.finished.len() == HISTORY_LEN {
                self.finished.pop_front();
            }
            self.finished.push_back(record);
        }
        self.phase = Phase::Idle;
        self.last_value = None;
        self.strip.clear();
    }

    /// Track the active workspace once a frame: a change starts the
    /// switch animation or retargets one already running from where it
    /// is drawn. `suspended` (the overview is up) settles at once.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        active: u32,
        workspaces: &[u32],
        output_width: i32,
        policy: MotionPolicy,
        suspended: bool,
        now: Instant,
    ) {
        self.policy = policy;
        self.distance = f64::from(output_width + WORKSPACE_SPACING);
        let previous = self.seen.replace(active);
        if suspended {
            if !matches!(self.phase, Phase::Idle) {
                self.archive(now);
            }
            return;
        }
        if self.following() {
            return;
        }
        let Some(previous) = previous.filter(|p| *p != active) else {
            return;
        };
        if policy.motion == Motion::Off {
            self.archive(now);
            self.record = Some(SlideRecord {
                from: previous,
                to: active,
                motion: Motion::Off,
                gesture: false,
                duration_ms: 0.0,
                elapsed_ms: 0.0,
                frames: 0,
                retargets: 0,
                reversed: false,
                velocity: 0.0,
                started: now,
            });
            self.archive(now);
            return;
        }
        // Retarget from where the strip is drawn now, never from where
        // the last motion began.
        self.last_value = self.value(now);
        self.merge_strip(workspaces, &[previous, active]);
        let Some(to) = self.slot(active) else {
            return;
        };
        let from = match (&self.phase, self.last_value) {
            (Phase::Easing { to: heading, .. }, _) if *heading == to => return,
            (Phase::Easing { .. }, Some(shown)) => shown,
            _ => self.slot(previous).unwrap_or(to),
        };
        let duration = policy.scaled(SWITCH_MS);
        match &mut self.record {
            Some(record) => {
                record.to = active;
                record.retargets += 1;
                record.duration_ms = duration.as_secs_f64() * 1000.0;
                record.motion = policy.motion;
            }
            None => {
                self.record = Some(SlideRecord {
                    from: previous,
                    to: active,
                    motion: policy.motion,
                    gesture: false,
                    duration_ms: duration.as_secs_f64() * 1000.0,
                    elapsed_ms: 0.0,
                    frames: 0,
                    retargets: 0,
                    reversed: false,
                    velocity: 0.0,
                    started: now,
                });
            }
        }
        self.last_value = Some(from);
        self.phase = Phase::Easing {
            from,
            to,
            start: now,
            duration,
        };
    }

    /// Advance to `now`; returns whether a motion is still drawn.
    pub fn step(&mut self, now: Instant) -> bool {
        self.frame = Some(now);
        let Some(value) = self.value(now) else {
            return false;
        };
        if let Phase::Easing {
            to,
            start,
            duration,
            ..
        } = &self.phase
        {
            let to = *to;
            let done = now.saturating_duration_since(*start) >= *duration;
            if let (Some(record), Some(last)) = (self.record.as_mut(), self.last_value) {
                record.frames += 1;
                if (value - last) * (to - last).signum() < -1e-9 {
                    record.reversed = true;
                }
            }
            self.last_value = Some(value);
            if done || self.policy.motion == Motion::Off {
                self.archive(now);
                return false;
            }
        } else if let Some(record) = self.record.as_mut() {
            record.frames += 1;
        }
        self.running()
    }

    /// Fingers went down: hold the strip where it is drawn (catching a
    /// switch still in flight) on the active workspace's strip.
    pub fn begin_swipe(
        &mut self,
        active: u32,
        workspaces: &[u32],
        output_width: i32,
        policy: MotionPolicy,
        now: Instant,
    ) {
        self.policy = policy;
        self.distance = f64::from(output_width + WORKSPACE_SPACING);
        self.last_value = self.value(now);
        // GNOME keeps an empty workspace after the last: swiping onto it
        // creates it.
        let trailing = workspaces
            .iter()
            .chain([&active])
            .max()
            .copied()
            .unwrap_or(0)
            + 1;
        self.merge_strip(workspaces, &[active, trailing]);
        let progress = match self.last_value {
            Some(shown) => shown,
            None => self.slot(active).unwrap_or(0.0),
        };
        let last = self.strip.len().saturating_sub(1) as f64;
        let initial = progress.round().clamp(0.0, last);
        let from = self.strip.get(initial as usize).copied().unwrap_or(active);
        if self.record.is_none() {
            self.record = Some(SlideRecord {
                from,
                to: from,
                motion: policy.motion,
                gesture: true,
                duration_ms: 0.0,
                elapsed_ms: 0.0,
                frames: 0,
                retargets: 0,
                reversed: false,
                velocity: 0.0,
                started: now,
            });
        } else if let Some(record) = self.record.as_mut() {
            record.gesture = true;
            record.retargets += 1;
        }
        self.last_value = Some(progress);
        self.phase = Phase::Following {
            progress,
            initial,
            history: SwipeHistory::default(),
        };
    }

    /// Fingers moved `dx` px (left is towards the next workspace).
    pub fn update_swipe(&mut self, dx: f64, time: u32) {
        let last = self.strip.len().saturating_sub(1) as f64;
        if let Phase::Following {
            progress,
            initial,
            history,
        } = &mut self.phase
        {
            let delta = -dx;
            history.append(time, delta);
            let (low, high) = ((*initial - 1.0).max(0.0), (*initial + 1.0).min(last));
            *progress = (*progress + delta / TOUCHPAD_BASE_WIDTH).clamp(low, high);
            self.last_value = Some(*progress);
        }
    }

    /// Fingers lifted: animate to where the swipe lands and return that
    /// workspace for the caller to activate.
    pub fn end_swipe(&mut self, cancelled: bool, time: u32, now: Instant) -> Option<u32> {
        let Phase::Following {
            progress,
            initial,
            history,
        } = &mut self.phase
        else {
            return None;
        };
        let velocity = history.velocity(time);
        let (progress, initial) = (*progress, *initial);
        let (end, ms) = swipe_release(progress, initial, self.strip.len(), velocity, cancelled);
        let target = self.strip.get(end as usize).copied();
        let duration = self.policy.scaled(ms);
        if let Some(record) = self.record.as_mut() {
            record.to = target.unwrap_or(record.to);
            record.duration_ms = duration.as_secs_f64() * 1000.0;
            record.velocity = velocity;
            record.motion = self.policy.motion;
        }
        self.phase = Phase::Easing {
            from: progress,
            to: end,
            start: now,
            duration,
        };
        if self.policy.motion == Motion::Off || duration.is_zero() {
            self.archive(now);
        }
        target
    }

    /// The in-flight motion and the last finished ones, for the state
    /// snapshot.
    pub fn snapshot(&self, now: Instant) -> serde_json::Value {
        let flight = self.value(now).map(|position| {
            let (to, from) = match &self.phase {
                Phase::Easing { to, from, .. } => (*to, *from),
                Phase::Following { initial, .. } => (*initial, *initial),
                Phase::Idle => (position, position),
            };
            serde_json::json!({
                "position": position,
                // How far the destination still is from the screen.
                "target_dx": ((to - position) * self.distance).round(),
                "from_slot": from,
                "to_slot": to,
                "strip": self.strip,
                "following": self.following(),
                "record": self.record.as_ref().map(SlideRecord::json),
            })
        });
        serde_json::json!({
            "in_flight": flight,
            "history": self.finished.iter().map(SlideRecord::json).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn ease_out_cubic_matches_clutter() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-12);
        assert_eq!(ease_out_cubic(2.0), 1.0);
    }

    #[test]
    fn a_key_switch_slides_both_workspaces_for_250_ms() {
        let t0 = Instant::now();
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::from_enabled(true);
        slide.observe(0, &[0], 1000, on, false, t0);
        assert!(!slide.running());
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        assert!(slide.running());
        // At the start the old workspace fills the screen, the new one
        // waits just off it, one output plus 100px to the right.
        assert_eq!(slide.placement(0, t0).unwrap().dx, 0);
        assert!(slide.placement(1, t0).is_none());
        let half = t0 + ms(125);
        let d0 = slide.placement(0, half).unwrap().dx;
        assert_eq!(d0, -(0.875f64 * 1100.0).round() as i32);
        assert!((slide.placement(1, half).unwrap().dx - (1100 + d0)).abs() <= 1);
        assert!(slide.step(half));
        assert!(!slide.step(t0 + ms(250)));
        assert!(!slide.running());
        let history = slide.snapshot(t0 + ms(250))["history"].clone();
        assert_eq!(history[0]["duration_ms"], 250.0);
        assert_eq!(history[0]["elapsed_ms"], 250.0);
        assert_eq!(history[0]["reversed"], false);
    }

    #[test]
    fn a_second_press_retargets_from_the_drawn_offset() {
        let t0 = Instant::now();
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::from_enabled(true);
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        let mid = t0 + ms(60);
        slide.step(mid);
        let shown = slide.value(mid).unwrap();
        assert!(shown > 0.0 && shown < 1.0);
        slide.observe(2, &[0, 1, 2], 1000, on, false, mid);
        assert!((slide.value(mid).unwrap() - shown).abs() < 1e-9);
        let mut last = shown;
        for step in 1..=25 {
            let now = mid + ms(step * 10);
            slide.step(now);
            if let Some(v) = slide.value(now) {
                assert!(v >= last);
                last = v;
            }
        }
        let record = &slide.snapshot(mid + ms(250))["history"][0];
        assert_eq!(record["from"], 0);
        assert_eq!(record["to"], 2);
        assert_eq!(record["retargets"], 1);
        assert_eq!(record["reversed"], false);
    }

    #[test]
    fn motion_off_switches_instantly_and_fade_crossfades_in_place() {
        let t0 = Instant::now();
        let mut slide = WorkspaceSlide::default();
        slide.observe(0, &[0], 1000, MotionPolicy::from_enabled(false), false, t0);
        slide.observe(
            1,
            &[0, 1],
            1000,
            MotionPolicy::from_enabled(false),
            false,
            t0,
        );
        assert!(!slide.running());
        assert_eq!(slide.snapshot(t0)["history"][0]["motion"], "off");
        let fade = MotionPolicy {
            motion: Motion::Fade,
            slowdown: 1.0,
        };
        slide.observe(2, &[0, 1, 2], 1000, fade, false, t0);
        let mid = t0 + ms(125);
        let out = slide.placement(1, mid).unwrap();
        let incoming = slide.placement(2, mid).unwrap();
        assert_eq!((out.dx, incoming.dx), (0, 0));
        assert!((out.alpha + incoming.alpha - 1.0).abs() < 1e-6);
        assert!(incoming.alpha > 0.8);
    }

    #[test]
    fn the_slowdown_factor_stretches_the_switch() {
        let t0 = Instant::now();
        let mut slide = WorkspaceSlide::default();
        let slow = MotionPolicy {
            motion: Motion::Slide,
            slowdown: 2.0,
        };
        slide.observe(0, &[0], 1000, slow, false, t0);
        slide.observe(1, &[0, 1], 1000, slow, false, t0);
        assert!(slide.step(t0 + ms(300)));
        assert!(!slide.step(t0 + ms(500)));
    }

    #[test]
    fn the_overview_settles_a_running_switch() {
        let t0 = Instant::now();
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::from_enabled(true);
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, true, t0 + ms(10));
        assert!(!slide.running());
    }

    #[test]
    fn velocity_is_the_last_150_ms_of_travel() {
        let mut history = SwipeHistory::default();
        history.append(1000, 10.0);
        history.append(1010, 20.0);
        history.append(1020, 20.0);
        assert_eq!(history.velocity(1020), 2.0);
        // A pause before lifting the fingers drops the old samples.
        assert_eq!(history.velocity(1400), 0.0);
    }

    #[test]
    fn slow_releases_snap_to_the_nearer_workspace() {
        // Under half way and slow: back where it began, 3x the base speed.
        let (end, duration) = swipe_release(0.3, 0.0, 3, 0.0, false);
        assert_eq!(end, 0.0);
        assert_eq!(duration, 400.0f64.min(0.3 / ANIMATION_BASE_VELOCITY * 3.0));
        let (end, duration) = swipe_release(0.6, 0.0, 3, 0.0, false);
        assert_eq!(end, 1.0);
        assert_eq!(duration, 400.0);
        let (end, _) = swipe_release(0.9, 0.0, 3, 0.0, true);
        assert_eq!(end, 0.0);
    }

    #[test]
    fn a_flick_completes_faster_than_a_drag() {
        // A short fast flick still lands one workspace over...
        let (end, fast) = swipe_release(0.1, 0.0, 3, 5.0, false);
        assert_eq!(end, 1.0);
        // ...and a faster one finishes sooner, never under 100 ms.
        let (_, faster) = swipe_release(0.1, 0.0, 3, 12.0, false);
        assert!(faster < fast);
        assert!(faster >= MIN_SWIPE_MS);
        assert!(fast <= MAX_SWIPE_MS);
        let (_, drag) = swipe_release(0.6, 0.0, 3, 0.0, false);
        assert!(fast < drag);
        // A flick back towards the start returns there.
        let (end, _) = swipe_release(1.4, 1.0, 3, -1.0, false);
        assert_eq!(end, 1.0);
        // Never past the neighbouring workspace.
        let (end, _) = swipe_release(1.0, 1.0, 5, 50.0, false);
        assert_eq!(end, 2.0);
    }

    #[test]
    fn a_swipe_follows_the_fingers_then_finishes_on_its_velocity() {
        let t0 = Instant::now();
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::from_enabled(true);
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.begin_swipe(0, &[0], 1000, on, t0);
        slide.update_swipe(-100.0, 1000);
        assert_eq!(slide.value(t0), Some(0.25));
        assert_eq!(slide.placement(1, t0).unwrap().dx, 825);
        slide.update_swipe(-40.0, 1010);
        slide.update_swipe(-40.0, 1020);
        let target = slide.end_swipe(false, 1020, t0);
        // Two workspaces and the trailing empty one: the flick creates it.
        assert_eq!(target, Some(1));
        // The model follows; the observed switch keeps the swipe's timing.
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        let record = slide.snapshot(t0)["in_flight"]["record"].clone();
        assert_eq!(record["gesture"], true);
        assert!(record["duration_ms"].as_f64().unwrap() < 250.0);
        assert!(!slide.step(t0 + ms(400)));
    }

    #[test]
    fn a_swipe_catches_a_switch_in_flight() {
        let t0 = Instant::now();
        let mut slide = WorkspaceSlide::default();
        let on = MotionPolicy::from_enabled(true);
        slide.observe(0, &[0], 1000, on, false, t0);
        slide.observe(1, &[0, 1], 1000, on, false, t0);
        let mid = t0 + ms(50);
        slide.step(mid);
        let shown = slide.value(mid).unwrap();
        slide.begin_swipe(1, &[0, 1], 1000, on, mid);
        assert!((slide.value(mid).unwrap() - shown).abs() < 1e-9);
        assert!(slide.following());
    }
}
