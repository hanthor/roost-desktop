//! GNOME 51's size-change transition (#496): maximize, unmaximize,
//! half-tiling and fullscreen (windowManager.js `_sizeChangeWindow` and
//! `_sizeChangedWindow`).
//!
//! GNOME freezes the window on its old frame when the layout changes,
//! waits for the client's buffer at the new size, then for 250 ms
//! (ease-out-quad) fades a clone of the old frame out while both the
//! clone and the window ease from the old rect to the new one. Here
//! that is all render-time: one offscreen snapshot of the old frame
//! per change, then per-frame rects and an alpha. The client is
//! configured once, as before, and the shell never repaints.
//!
//! One deliberate departure: GNOME starts the new buffer at
//! `1 / scale`, which stretches a shrinking window's new content up to
//! the old size. Here the live window's scale is clamped to 1 per axis,
//! so no frame shows new content larger than its final size; the
//! fading snapshot covers the difference.
//!
//! The state machine is generic over the snapshot so it can be tested
//! without a GPU; [`step_frame`] is the runtime glue that renders the
//! snapshots and feeds it the windows' committed sizes.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use smithay::utils::{Logical, Rectangle, Size};

pub type Rect = Rectangle<i32, Logical>;
pub type RectF = Rectangle<f64, Logical>;

/// windowManager.js `WINDOW_ANIMATION_TIME`.
pub const DURATION: Duration = Duration::from_millis(250);
/// How long a frozen window waits for the client's buffer at its new
/// size. GNOME waits for `size-changed` with no bound; a hung client
/// here gets its old frame for at most this long, then the window
/// shows at its new place without a transition.
pub const BUFFER_TIMEOUT: Duration = Duration::from_millis(500);
/// Finished transitions kept for the state document.
const HISTORY: usize = 8;
/// Frame samples kept per transition (a 250 ms run at 60 Hz is ~16).
const MAX_SAMPLES: usize = 64;

/// How much of a transition plays: the shared motion policy's three
/// states (#493). Fade-only is GNOME's reduced motion: the clone fades
/// where the old frame was and the window is at its new place at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Full,
    FadeOnly,
    Off,
}

/// What the session's motion policy allows a transition, and how long
/// it runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    pub motion: Motion,
    pub duration: Duration,
}

impl Policy {
    /// From the shared motion policy (#493): the scale and translation
    /// need `allows_motion`, the crossfade `allows_fades`, and the
    /// 250 ms go through GNOME's `adjustAnimationTime`.
    pub fn from_motion(policy: tuna_shell_control::motion::MotionPolicy) -> Self {
        let motion = if policy.allows_motion() {
            Motion::Full
        } else if policy.allows_fades() {
            Motion::FadeOnly
        } else {
            Motion::Off
        };
        let ms = policy.adjust_ms(DURATION.as_secs_f64() * 1000.0);
        Self {
            motion,
            duration: Duration::from_secs_f64(ms.max(0.0) / 1000.0),
        }
    }

    pub fn duration(&self) -> Duration {
        self.duration
    }
}

/// What one frame draws for a window in transition, in global logical
/// coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Whether the live window draws (not while frozen on the old frame).
    pub live_visible: bool,
    /// Where the live window's frame (its xdg geometry) is drawn: the
    /// committed size times `live_scale`.
    pub live: RectF,
    /// Per-axis scale of the live window, never above 1.
    pub live_scale: (f64, f64),
    /// Where the old-frame snapshot is drawn, stretched to this rect.
    pub snapshot: RectF,
    pub snapshot_alpha: f32,
}

/// GNOME's `EASE_OUT_QUAD`.
pub fn ease_out_quad(t: f64) -> f64 {
    crate::overview::ease_out_quad(t)
}

fn lerp(a: f64, b: f64, p: f64) -> f64 {
    a + (b - a) * p
}

/// The rect `p` of the way from `from` to `to`.
pub fn lerp_rect(from: Rect, to: Rect, p: f64) -> RectF {
    let (from, to) = (from.to_f64(), to.to_f64());
    Rectangle::new(
        (lerp(from.loc.x, to.loc.x, p), lerp(from.loc.y, to.loc.y, p)).into(),
        (
            lerp(from.size.w, to.size.w, p),
            lerp(from.size.h, to.size.h, p),
        )
            .into(),
    )
}

/// The frame while frozen: the old frame where it was, the window hidden.
pub fn frozen_frame(from: Rect) -> Frame {
    Frame {
        live_visible: false,
        live: from.to_f64(),
        live_scale: (1.0, 1.0),
        snapshot: from.to_f64(),
        snapshot_alpha: 1.0,
    }
}

/// The frame `progress` (linear 0..1) through a transition from the
/// old frame `from` to the final frame `to` (the new position at the
/// client's committed size).
pub fn frame_at(from: Rect, to: Rect, progress: f64, motion: Motion) -> Frame {
    let p = ease_out_quad(progress);
    let shown = match motion {
        Motion::Full => lerp_rect(from, to, p),
        Motion::FadeOnly | Motion::Off => to.to_f64(),
    };
    let axis = |shown: f64, last: i32| {
        if last > 0 {
            (shown / f64::from(last)).clamp(0.0, 1.0)
        } else {
            1.0
        }
    };
    let live_scale = (axis(shown.size.w, to.size.w), axis(shown.size.h, to.size.h));
    let live = Rectangle::new(
        shown.loc,
        (
            f64::from(to.size.w) * live_scale.0,
            f64::from(to.size.h) * live_scale.1,
        )
            .into(),
    );
    let snapshot = match motion {
        Motion::Full => shown,
        Motion::FadeOnly | Motion::Off => from.to_f64(),
    };
    Frame {
        live_visible: true,
        live,
        live_scale,
        snapshot,
        snapshot_alpha: (1.0 - p).clamp(0.0, 1.0) as f32,
    }
}

/// A layout change the window manager made, for the runtime to start
/// (or skip) a transition for at the next frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    /// The window's geometry before the change.
    pub old: Rect,
    /// Its geometry after.
    pub new: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Frozen on the old frame until the client commits its new size.
    Waiting {
        since: Duration,
    },
    Animating {
        start: Duration,
    },
}

/// How a transition ended, for the state document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    /// The client's new-size buffer never came within [`BUFFER_TIMEOUT`].
    TimedOut,
    /// A newer change on the same window took over mid-flight.
    Replaced,
    /// The window went away (or left the screen) mid-flight.
    Cancelled,
    /// Never started: `reason` says why.
    Skipped(&'static str),
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Completed => "completed",
            Outcome::TimedOut => "timed_out",
            Outcome::Replaced => "replaced",
            Outcome::Cancelled => "cancelled",
            Outcome::Skipped(_) => "skipped",
        }
    }
}

/// One drawn frame of a transition: milliseconds since it started
/// moving, the live window's rect and the snapshot's alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub t_ms: f64,
    pub live: Rect,
    pub alpha: f32,
}

/// One transition's account, kept after it ends.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub seq: u64,
    pub window: u64,
    pub from: Rect,
    pub to: Rect,
    /// The final frame: the new position at the committed size.
    pub last: Rect,
    pub outcome: Option<Outcome>,
    /// How long it stayed frozen waiting for the client.
    pub waited_ms: Option<f64>,
    /// From the first moving frame to the frame that settled.
    pub settle_ms: Option<f64>,
    pub samples: Vec<Sample>,
}

struct Entry<T> {
    from: Rect,
    to: Rect,
    /// The committed size the snapshot was taken at.
    old_size: Size<i32, Logical>,
    snapshot: T,
    phase: Phase,
    frame: Frame,
    record: Record,
}

/// What [`SizeChanges::step`] needs to know about a window this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Live {
    /// Its target geometry (where the window manager has it).
    pub target: Rect,
    /// The size its client last committed.
    pub committed: Size<i32, Logical>,
}

/// Every window's size-change transition, plus the queue of layout
/// changes the runtime has yet to look at.
pub struct SizeChanges<T> {
    requests: Vec<Request>,
    entries: HashMap<u64, Entry<T>>,
    history: VecDeque<Record>,
    seq: u64,
    duration: Duration,
}

impl<T> Default for SizeChanges<T> {
    fn default() -> Self {
        Self {
            requests: Vec::new(),
            entries: HashMap::new(),
            history: VecDeque::new(),
            seq: 0,
            duration: DURATION,
        }
    }
}

impl<T> SizeChanges<T> {
    /// Queue a layout change. Several before one frame fold into one:
    /// the first old geometry, the last new one.
    pub fn request(&mut self, id: u64, old: Rect, new: Rect) {
        if let Some(pending) = self.requests.iter_mut().find(|r| r.id == id) {
            pending.new = new;
        } else {
            self.requests.push(Request { id, old, new });
        }
    }

    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// Whether `id` is frozen waiting for its client.
    pub fn is_waiting(&self, id: u64) -> bool {
        self.entries
            .get(&id)
            .is_some_and(|e| matches!(e.phase, Phase::Waiting { .. }))
    }

    /// Where `id`'s live window is drawn now, when in transition: a new
    /// change mid-flight starts from there.
    pub fn drawn_rect(&self, id: u64) -> Option<Rect> {
        self.entries.get(&id).map(|e| round(e.frame.live))
    }

    /// This frame's drawing for `id` and its old-frame snapshot.
    pub fn frame(&self, id: u64) -> Option<(Frame, &T)> {
        self.entries.get(&id).map(|e| (e.frame, &e.snapshot))
    }

    pub fn in_flight(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Point a frozen transition at a newer target: the old frame and
    /// its snapshot stay, the wait starts over.
    pub fn retarget(&mut self, id: u64, to: Rect, now: Duration) -> bool {
        match self.entries.get_mut(&id) {
            Some(entry) if matches!(entry.phase, Phase::Waiting { .. }) => {
                entry.to = to;
                entry.record.to = to;
                entry.record.last = to;
                entry.phase = Phase::Waiting { since: now };
                true
            }
            _ => false,
        }
    }

    /// Freeze `id` on its old frame `from` (`old_size` committed,
    /// `snapshot` of it) until the client commits the size for `to`.
    /// A window whose size does not change moves at once.
    pub fn begin(
        &mut self,
        id: u64,
        from: Rect,
        to: Rect,
        old_size: Size<i32, Logical>,
        snapshot: T,
        now: Duration,
    ) {
        self.end(id, Outcome::Replaced, now);
        self.seq += 1;
        let phase = if old_size == to.size {
            Phase::Animating { start: now }
        } else {
            Phase::Waiting { since: now }
        };
        let record = Record {
            seq: self.seq,
            window: id,
            from,
            to,
            last: to,
            outcome: None,
            waited_ms: matches!(phase, Phase::Animating { .. }).then_some(0.0),
            settle_ms: None,
            samples: Vec::new(),
        };
        self.entries.insert(
            id,
            Entry {
                from,
                to,
                old_size,
                snapshot,
                phase,
                frame: frozen_frame(from),
                record,
            },
        );
    }

    /// Record a change that gets no transition, and drop any running
    /// one for the window (it lands where it is going).
    pub fn skip(&mut self, id: u64, from: Rect, to: Rect, reason: &'static str, now: Duration) {
        self.end(id, Outcome::Replaced, now);
        self.seq += 1;
        self.push_history(Record {
            seq: self.seq,
            window: id,
            from,
            to,
            last: to,
            outcome: Some(Outcome::Skipped(reason)),
            waited_ms: None,
            settle_ms: None,
            samples: Vec::new(),
        });
    }

    /// End `id`'s transition (if any) with `outcome`.
    pub fn end(&mut self, id: u64, outcome: Outcome, now: Duration) {
        if let Some(mut entry) = self.entries.remove(&id) {
            if let (Phase::Animating { start }, Outcome::Completed) = (entry.phase, outcome) {
                entry.record.settle_ms = Some(ms(now.saturating_sub(start)));
            }
            entry.record.outcome = Some(outcome);
            self.push_history(entry.record);
        }
    }

    /// Advance every transition to `now` under `policy`. `live` gives
    /// each window's target and committed size, `None` when it is gone
    /// or off screen. Returns whether any transition is still running.
    pub fn step(
        &mut self,
        now: Duration,
        policy: Policy,
        live: impl Fn(u64) -> Option<Live>,
    ) -> bool {
        self.duration = policy.duration();
        let ids: Vec<u64> = self.entries.keys().copied().collect();
        for id in ids {
            let Some(info) = live(id) else {
                self.end(id, Outcome::Cancelled, now);
                continue;
            };
            if policy.motion == Motion::Off {
                // A live switch to off lands everything in flight.
                self.end(id, Outcome::Completed, now);
                continue;
            }
            let Some(entry) = self.entries.get_mut(&id) else {
                continue;
            };
            entry.to = info.target;
            if let Phase::Waiting { since } = entry.phase {
                if info.committed != entry.old_size {
                    entry.record.waited_ms = Some(ms(now.saturating_sub(since)));
                    entry.phase = Phase::Animating { start: now };
                } else if now.saturating_sub(since) > BUFFER_TIMEOUT {
                    self.end(id, Outcome::TimedOut, now);
                    continue;
                } else {
                    entry.frame = frozen_frame(entry.from);
                    continue;
                }
            }
            let Phase::Animating { start } = entry.phase else {
                continue;
            };
            let last = Rect::new(info.target.loc, info.committed);
            let elapsed = now.saturating_sub(start);
            let progress = elapsed.as_secs_f64() / self.duration.as_secs_f64().max(1e-9);
            if progress >= 1.0 {
                entry.record.last = last;
                self.end(id, Outcome::Completed, now);
                continue;
            }
            entry.frame = frame_at(entry.from, last, progress, policy.motion);
            entry.record.last = last;
            if entry.record.samples.len() < MAX_SAMPLES {
                entry.record.samples.push(Sample {
                    t_ms: ms(elapsed),
                    live: round(entry.frame.live),
                    alpha: entry.frame.snapshot_alpha,
                });
            }
        }
        self.in_flight()
    }

    fn push_history(&mut self, record: Record) {
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(record);
    }

    pub fn history(&self) -> impl Iterator<Item = &Record> {
        self.history.iter()
    }

    /// The state document's `size_changes` object: transitions in
    /// flight with this frame's rects, and the last few finished ones
    /// with every sampled frame.
    pub fn to_json(&self) -> serde_json::Value {
        let r = |r: Rect| [r.loc.x, r.loc.y, r.size.w, r.size.h];
        let rf = |r: RectF| [r.loc.x, r.loc.y, r.size.w, r.size.h];
        let mut in_flight: Vec<_> = self.entries.iter().collect();
        in_flight.sort_by_key(|(id, _)| **id);
        let in_flight: Vec<_> = in_flight
            .into_iter()
            .map(|(id, e)| {
                serde_json::json!({
                    "window": id,
                    "seq": e.record.seq,
                    "phase": match e.phase {
                        Phase::Waiting { .. } => "waiting",
                        Phase::Animating { .. } => "animating",
                    },
                    "from": r(e.from),
                    "to": r(e.to),
                    "live_visible": e.frame.live_visible,
                    "live": rf(e.frame.live),
                    "live_scale": [e.frame.live_scale.0, e.frame.live_scale.1],
                    "snapshot": rf(e.frame.snapshot),
                    "snapshot_alpha": e.frame.snapshot_alpha,
                })
            })
            .collect();
        let history: Vec<_> = self
            .history
            .iter()
            .map(|h| {
                serde_json::json!({
                    "seq": h.seq,
                    "window": h.window,
                    "from": r(h.from),
                    "to": r(h.to),
                    "last": r(h.last),
                    "outcome": h.outcome.map(Outcome::as_str),
                    "reason": match h.outcome {
                        Some(Outcome::Skipped(reason)) => Some(reason),
                        _ => None,
                    },
                    "waited_ms": h.waited_ms,
                    "settle_ms": h.settle_ms,
                    "samples": h.samples.iter().map(|s| serde_json::json!({
                        "t_ms": s.t_ms, "live": r(s.live), "alpha": s.alpha,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::json!({
            "duration_ms": ms(self.duration),
            "timeout_ms": ms(BUFFER_TIMEOUT),
            "last_seq": self.seq,
            "in_flight": in_flight,
            "history": history,
        })
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn round(r: RectF) -> Rect {
    Rect::new(
        (r.loc.x.round() as i32, r.loc.y.round() as i32).into(),
        (r.size.w.round() as i32, r.size.h.round() as i32).into(),
    )
}

/// An old frame kept as a texture, with a stable element id so damage
/// tracking sees one element move and fade.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub texture: smithay::backend::renderer::gles::GlesTexture,
    pub id: smithay::backend::renderer::element::Id,
}

/// The snapshot drawn for one output (`view`) as a texture element in
/// physical pixels; draw it at scale 1.
pub fn snapshot_element(
    renderer: &smithay::backend::renderer::gles::GlesRenderer,
    snapshot: &Snapshot,
    frame: &Frame,
    view: crate::runtime::View,
) -> Option<
    smithay::backend::renderer::element::texture::TextureRenderElement<
        smithay::backend::renderer::gles::GlesTexture,
    >,
> {
    use smithay::backend::renderer::element::texture::TextureRenderElement;
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::Renderer;
    if frame.snapshot_alpha <= 0.0 {
        return None;
    }
    let r = frame.snapshot;
    let start = view.physical(r.loc.x, r.loc.y);
    let end = view.physical(r.loc.x + r.size.w, r.loc.y + r.size.h);
    let (w, h) = (end.x - start.x, end.y - start.y);
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(TextureRenderElement::from_static_texture(
        snapshot.id.clone(),
        renderer.context_id(),
        (f64::from(start.x), f64::from(start.y)),
        snapshot.texture.clone(),
        1,
        smithay::utils::Transform::Normal,
        Some(frame.snapshot_alpha),
        None,
        Some((w, h).into()),
        None,
        Kind::Unspecified,
    ))
}

/// Render `surface`'s window (its xdg geometry, without shadows or
/// popups) into a texture of `size` logical pixels at `scale`.
fn render_snapshot(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    size: Size<i32, Logical>,
    scale: f64,
) -> Option<smithay::backend::renderer::gles::GlesTexture> {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::element::surface::{
        render_elements_from_surface_tree, WaylandSurfaceRenderElement,
    };
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::utils::draw_render_elements;
    use smithay::backend::renderer::{Bind, Color32F, Frame as _, Offscreen, Renderer};
    use smithay::utils::{Physical, Transform};
    let px = |v: i32| ((f64::from(v) * scale).round() as i32).max(1);
    let physical: Size<i32, Physical> = (px(size.w), px(size.h)).into();
    let geo = crate::popup::window_geometry_loc(surface);
    let origin: smithay::utils::Point<i32, Physical> = (
        (-f64::from(geo.x) * scale).round() as i32,
        (-f64::from(geo.y) * scale).round() as i32,
    )
        .into();
    let elements: Vec<WaylandSurfaceRenderElement<_>> =
        render_elements_from_surface_tree(renderer, surface, origin, scale, 1.0, Kind::Unspecified);
    let mut texture: smithay::backend::renderer::gles::GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (physical.w, physical.h).into())
        .ok()?;
    {
        let mut target = renderer.bind(&mut texture).ok()?;
        let mut frame = renderer
            .render(&mut target, physical, Transform::Normal)
            .ok()?;
        let all = Rectangle::from_size(physical);
        frame
            .clear(Color32F::new(0.0, 0.0, 0.0, 0.0), &[all])
            .ok()?;
        draw_render_elements(&mut frame, scale, &elements, &[all]).ok()?;
        let _ = frame.finish().ok()?;
    }
    Some(texture)
}

/// Once per frame, before the scene is built: start transitions for
/// the layout changes queued since the last frame (snapshotting each
/// old frame) and advance the running ones. `allowed` is false while
/// the overview is open or a workspace swipe is held (GNOME's
/// `_shouldAnimate`). Returns whether a transition is in flight.
pub fn step_frame(
    manager: &mut crate::windows::WindowManager,
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    state: &crate::State,
    now: Duration,
    policy: Policy,
    allowed: bool,
) -> bool {
    use crate::windows::committed_size;
    use smithay::wayland::seat::WaylandFocus;
    for request in manager.size_changes_mut().take_requests() {
        let id = request.id;
        let surface = manager.surface_of(id);
        let committed = surface.as_ref().and_then(committed_size);
        let reason = if policy.motion == Motion::Off {
            Some("animations-off")
        } else if !allowed {
            Some("overview-or-swipe")
        } else if committed.is_none_or(|size| size.w <= 0 || size.h <= 0) {
            Some("no-buffer")
        } else {
            None
        };
        if let Some(reason) = reason {
            manager
                .size_changes_mut()
                .skip(id, request.old, request.new, reason, now);
            continue;
        }
        if manager.size_changes_mut().retarget(id, request.new, now) {
            continue;
        }
        let (Some(surface), Some(committed)) = (surface, committed) else {
            continue;
        };
        // The frame drawn now: mid-flight, where the live window is;
        // otherwise the old position at the size the client last drew.
        let from = manager
            .size_changes()
            .drawn_rect(id)
            .unwrap_or(Rect::new(request.old.loc, committed));
        let scale = state.scale_for(from).fractional_scale();
        let Some(texture) = render_snapshot(renderer, &surface, committed, scale) else {
            manager
                .size_changes_mut()
                .skip(id, request.old, request.new, "snapshot-failed", now);
            continue;
        };
        let snapshot = Snapshot {
            texture,
            id: smithay::backend::renderer::element::Id::new(),
        };
        manager
            .size_changes_mut()
            .begin(id, from, request.new, committed, snapshot, now);
    }
    if !manager.size_changes().in_flight() {
        return false;
    }
    let shown: HashMap<u64, Live> = manager
        .render_entries()
        .into_iter()
        .filter_map(|(id, window, target)| {
            let committed = committed_size(window.wl_surface()?.as_ref())?;
            Some((id, Live { target, committed }))
        })
        .collect();
    manager
        .size_changes_mut()
        .step(now, policy, |id| shown.get(&id).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect::new((x, y).into(), (w, h).into())
    }
    fn size(w: i32, h: i32) -> Size<i32, Logical> {
        (w, h).into()
    }
    const FLOATING: (i32, i32, i32, i32) = (300, 200, 400, 300);
    fn floating() -> Rect {
        rect(FLOATING.0, FLOATING.1, FLOATING.2, FLOATING.3)
    }
    fn maximized() -> Rect {
        rect(0, 32, 1280, 688)
    }
    fn full_policy() -> Policy {
        Policy::from_motion(Default::default())
    }
    fn off() -> Policy {
        Policy::from_motion(tuna_shell_control::motion::MotionPolicy::from_gnome(
            false, false, 1.0,
        ))
    }
    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }
    fn live(target: Rect, committed: Size<i32, Logical>) -> impl Fn(u64) -> Option<Live> {
        move |_| Some(Live { target, committed })
    }

    #[test]
    fn ease_out_quad_matches_clutter() {
        assert_eq!(ease_out_quad(0.0), 0.0);
        assert_eq!(ease_out_quad(0.5), 0.75);
        assert_eq!(ease_out_quad(1.0), 1.0);
        assert_eq!(ease_out_quad(2.0), 1.0);
        assert_eq!(ease_out_quad(-1.0), 0.0);
        // Monotonic and decelerating.
        let steps: Vec<f64> = (0..=10)
            .map(|i| ease_out_quad(f64::from(i) / 10.0))
            .collect();
        for pair in steps.windows(3) {
            assert!(pair[1] > pair[0]);
            assert!(pair[2] - pair[1] < pair[1] - pair[0]);
        }
    }

    #[test]
    fn the_shared_motion_policy_sets_the_level_and_duration() {
        use tuna_shell_control::motion::{MotionLevel, MotionPolicy};
        let full = Policy::from_motion(MotionPolicy::default());
        assert_eq!((full.motion, full.duration()), (Motion::Full, DURATION));
        let slow = Policy::from_motion(MotionPolicy::new(MotionLevel::Full, 2.0));
        assert_eq!(slow.duration(), ms(500));
        // Reduced motion keeps the crossfade, at the adjusted duration.
        let reduced = Policy::from_motion(MotionPolicy::from_gnome(true, true, 1.0));
        assert_eq!(
            (reduced.motion, reduced.duration()),
            (Motion::FadeOnly, DURATION)
        );
        assert_eq!(off().motion, Motion::Off);
    }

    #[test]
    fn growing_scales_the_new_buffer_up_to_its_size_never_past_it() {
        let (from, to) = (floating(), maximized());
        let start = frame_at(from, to, 0.0, Motion::Full);
        assert_eq!(start.live, from.to_f64());
        assert_eq!(start.snapshot, from.to_f64());
        assert_eq!(start.snapshot_alpha, 1.0);
        let mid = frame_at(from, to, 0.5, Motion::Full);
        // ease_out_quad(0.5) = 0.75 of the way.
        assert_eq!(mid.live, lerp_rect(from, to, 0.75));
        assert_eq!(mid.snapshot, mid.live);
        assert_eq!(mid.snapshot_alpha, 0.25);
        let end = frame_at(from, to, 1.0, Motion::Full);
        assert_eq!(end.live, to.to_f64());
        assert_eq!(end.live_scale, (1.0, 1.0));
        assert_eq!(end.snapshot_alpha, 0.0);
    }

    #[test]
    fn no_frame_stretches_new_content_beyond_its_final_size() {
        // Unmaximize, half-tile from maximized and tile to fullscreen:
        // shrinking, mixed and growing axes.
        let cases = [
            (maximized(), floating()),
            (maximized(), rect(0, 32, 640, 688)),
            (rect(640, 32, 640, 688), rect(0, 0, 1280, 720)),
            (floating(), maximized()),
        ];
        for (from, to) in cases {
            for motion in [Motion::Full, Motion::FadeOnly] {
                for i in 0..=20 {
                    let f = frame_at(from, to, f64::from(i) / 20.0, motion);
                    assert!(f.live_scale.0 <= 1.0 && f.live_scale.1 <= 1.0);
                    assert!(f.live.size.w <= f64::from(to.size.w) + 1e-9);
                    assert!(f.live.size.h <= f64::from(to.size.h) + 1e-9);
                }
            }
        }
        // Shrinking: the new buffer stays at its size and travels; the
        // snapshot shrinks over it and fades.
        let mid = frame_at(maximized(), floating(), 0.5, Motion::Full);
        assert_eq!(mid.live_scale, (1.0, 1.0));
        assert_eq!(mid.live.loc, lerp_rect(maximized(), floating(), 0.75).loc);
        assert_eq!(mid.live.size, floating().size.to_f64());
        assert_eq!(mid.snapshot, lerp_rect(maximized(), floating(), 0.75));
    }

    #[test]
    fn fade_only_puts_the_window_in_place_and_fades_the_old_frame_where_it_was() {
        let (from, to) = (floating(), maximized());
        for i in 0..=4 {
            let f = frame_at(from, to, f64::from(i) / 4.0, Motion::FadeOnly);
            assert_eq!(f.live, to.to_f64());
            assert_eq!(f.live_scale, (1.0, 1.0));
            assert_eq!(f.snapshot, from.to_f64());
        }
        assert_eq!(
            frame_at(from, to, 0.5, Motion::FadeOnly).snapshot_alpha,
            0.25
        );
    }

    #[test]
    fn requests_fold_per_window_until_taken() {
        let mut changes = SizeChanges::<()>::default();
        changes.request(1, floating(), maximized());
        changes.request(1, maximized(), rect(0, 32, 640, 688));
        changes.request(2, floating(), maximized());
        let taken = changes.take_requests();
        assert_eq!(
            taken,
            [
                Request {
                    id: 1,
                    old: floating(),
                    new: rect(0, 32, 640, 688)
                },
                Request {
                    id: 2,
                    old: floating(),
                    new: maximized()
                },
            ]
        );
        assert!(changes.take_requests().is_empty());
    }

    #[test]
    fn waits_frozen_for_the_new_buffer_then_eases_for_250_ms() {
        let full = full_policy();
        let mut changes = SizeChanges::default();
        let old = floating().size;
        changes.begin(7, floating(), maximized(), old, (), ms(1000));
        // The client has not drawn the new size yet: frozen on the old frame.
        assert!(changes.step(ms(1040), full, live(maximized(), old)));
        let (frame, _) = changes.frame(7).unwrap();
        assert!(!frame.live_visible);
        assert_eq!(frame.snapshot, floating().to_f64());
        assert_eq!(frame.snapshot_alpha, 1.0);
        // The buffer lands at 1080 ms: the clock starts there.
        assert!(changes.step(ms(1080), full, live(maximized(), maximized().size)));
        let (frame, _) = changes.frame(7).unwrap();
        assert!(frame.live_visible);
        assert_eq!(frame.live, floating().to_f64());
        assert!(changes.step(ms(1205), full, live(maximized(), maximized().size)));
        let (frame, _) = changes.frame(7).unwrap();
        assert_eq!(frame.live, lerp_rect(floating(), maximized(), 0.75));
        assert_eq!(frame.snapshot_alpha, 0.25);
        assert!(!changes.step(ms(1330), full, live(maximized(), maximized().size)));
        assert!(changes.frame(7).is_none());
        let record = changes.history().last().unwrap();
        assert_eq!(record.outcome, Some(Outcome::Completed));
        assert_eq!(record.waited_ms, Some(80.0));
        assert_eq!(record.settle_ms, Some(250.0));
        assert_eq!(record.last, maximized());
        assert_eq!(record.samples.len(), 2);
        assert_eq!(
            record.samples[1].live,
            round(lerp_rect(floating(), maximized(), 0.75))
        );
    }

    #[test]
    fn the_final_frame_is_the_size_the_client_chose() {
        let full = full_policy();
        let mut changes = SizeChanges::default();
        changes.begin(1, floating(), maximized(), floating().size, (), ms(0));
        // A terminal snaps to its cell grid, short of the work area.
        let chosen = size(1274, 680);
        changes.step(ms(10), full, live(maximized(), chosen));
        changes.step(ms(10 + 249), full, live(maximized(), chosen));
        let (frame, _) = changes.frame(1).unwrap();
        assert!(frame.live.size.w <= 1274.0 && frame.live.size.h <= 680.0);
        changes.step(ms(10 + 250), full, live(maximized(), chosen));
        let record = changes.history().last().unwrap();
        assert_eq!(record.last, Rect::new(maximized().loc, chosen));
    }

    #[test]
    fn a_client_that_never_resizes_times_out_and_lands() {
        let full = full_policy();
        let mut changes = SizeChanges::default();
        let old = floating().size;
        changes.begin(1, floating(), maximized(), old, (), ms(0));
        assert!(changes.step(ms(500), full, live(maximized(), old)));
        assert!(!changes.step(ms(501), full, live(maximized(), old)));
        let record = changes.history().last().unwrap();
        assert_eq!(record.outcome, Some(Outcome::TimedOut));
        assert!(record.samples.is_empty());
    }

    #[test]
    fn a_move_without_a_size_change_eases_at_once() {
        let full = full_policy();
        let mut changes = SizeChanges::default();
        let left = rect(0, 32, 640, 688);
        let right = rect(640, 32, 640, 688);
        changes.begin(1, left, right, left.size, (), ms(0));
        changes.step(ms(125), full, live(right, right.size));
        let (frame, _) = changes.frame(1).unwrap();
        assert!(frame.live_visible);
        assert_eq!(frame.live, lerp_rect(left, right, 0.75));
    }

    #[test]
    fn a_frozen_window_retargets_and_a_moving_one_is_replaced() {
        let full = full_policy();
        let mut changes = SizeChanges::default();
        let half = rect(0, 32, 640, 688);
        changes.begin(1, floating(), maximized(), floating().size, (), ms(0));
        assert!(changes.retarget(1, half, ms(400)));
        // The wait restarted with the retarget.
        assert!(changes.step(ms(800), full, live(half, floating().size)));
        assert!(changes.is_waiting(1));
        changes.step(ms(810), full, live(half, half.size));
        assert!(!changes.retarget(1, maximized(), ms(820)));
        let drawn = changes.drawn_rect(1).unwrap();
        changes.begin(1, drawn, maximized(), half.size, (), ms(820));
        let outcomes: Vec<_> = changes.history().map(|r| r.outcome).collect();
        assert_eq!(outcomes, [Some(Outcome::Replaced)]);
        assert!(changes.is_waiting(1));
    }

    #[test]
    fn gone_windows_cancel_and_switching_animations_off_lands_them() {
        let full = full_policy();
        let mut changes = SizeChanges::default();
        changes.begin(1, floating(), maximized(), floating().size, (), ms(0));
        changes.begin(2, floating(), maximized(), floating().size, (), ms(0));
        assert!(changes.step(ms(10), full, |id| (id == 2).then_some(Live {
            target: maximized(),
            committed: maximized().size,
        })));
        assert!(!changes.step(ms(20), off(), live(maximized(), maximized().size)));
        let outcomes: Vec<_> = changes.history().map(|r| (r.window, r.outcome)).collect();
        assert_eq!(
            outcomes,
            [(1, Some(Outcome::Cancelled)), (2, Some(Outcome::Completed))]
        );
    }

    #[test]
    fn skipped_changes_are_recorded_and_history_is_bounded() {
        let mut changes = SizeChanges::<()>::default();
        for i in 0..20 {
            changes.skip(i, floating(), maximized(), "animations-off", ms(0));
        }
        assert_eq!(changes.history().count(), HISTORY);
        let doc = changes.to_json();
        assert_eq!(doc["last_seq"], 20);
        assert_eq!(doc["history"][7]["outcome"], "skipped");
        assert_eq!(doc["history"][7]["reason"], "animations-off");
        assert_eq!(doc["in_flight"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn the_state_document_shows_the_frame_in_flight() {
        let full = full_policy();
        let mut changes = SizeChanges::default();
        changes.begin(3, floating(), maximized(), floating().size, (), ms(0));
        changes.step(ms(5), full, live(maximized(), maximized().size));
        changes.step(ms(130), full, live(maximized(), maximized().size));
        let doc = changes.to_json();
        let entry = &doc["in_flight"][0];
        assert_eq!(entry["window"], 3);
        assert_eq!(entry["phase"], "animating");
        assert_eq!(entry["snapshot_alpha"], 0.25);
        assert_eq!(doc["duration_ms"], 250.0);
        assert_eq!(doc["timeout_ms"], 500.0);
    }
}
