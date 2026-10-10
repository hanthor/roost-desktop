//! Window open/close and modal-dim effects (GNOME 51 `windowManager.js`
//! `_mapWindow`, `_destroyWindow` and `WindowDimmer`), drawn as
//! render-time transforms: opacity, scale about GNOME's pivot and a
//! brightness multiplier. Clients are never reconfigured and nothing
//! here asks the shell to repaint.
//!
//! A closing window keeps its last textures ([`Snapshot`]), captured
//! before its surface lets them go, and plays the close effect from
//! them after it has left the model, so it takes no input. Every
//! effect is split into a fade part and a motion part (scale and
//! translation): reduced motion ([`MotionLevel::FadeOnly`]) keeps the
//! first and drops the second, as GNOME does. Durations go through the
//! session's [`MotionPolicy::adjust_ms`] (GNOME's slow-down factor).

use std::collections::{HashMap, HashSet, VecDeque};

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ContextId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};

/// `SHOW_WINDOW_ANIMATION_TIME` (normal window map), seconds.
pub const SHOW_WINDOW: f64 = 0.150;
/// `DIALOG_SHOW_WINDOW_ANIMATION_TIME`.
pub const DIALOG_SHOW_WINDOW: f64 = 0.100;
/// `DESTROY_WINDOW_ANIMATION_TIME`.
pub const DESTROY_WINDOW: f64 = 0.150;
/// `DIALOG_DESTROY_WINDOW_ANIMATION_TIME`.
pub const DIALOG_DESTROY_WINDOW: f64 = 0.100;
/// `DIM_TIME` and `UNDIM_TIME`.
pub const DIM: f64 = 0.500;
pub const UNDIM: f64 = 0.250;
/// `DIM_BRIGHTNESS`: Clutter's brightness effect multiplies colour by
/// `1 + brightness` for a negative brightness.
pub const DIM_BRIGHTNESS: f64 = -0.3;
/// The dimmed parent's colour multiplier.
pub const DIMMED: f64 = 1.0 + DIM_BRIGHTNESS;
/// Finished effects kept for the state file (proof gates read them).
const LOG_LEN: usize = 16;

pub use tuna_shell_control::motion::{MotionLevel, MotionPolicy};

/// GNOME's `adjustAnimationTime` for a duration in seconds.
fn adjusted(policy: MotionPolicy, seconds: f64) -> f64 {
    policy.adjust_ms(seconds * 1000.0) / 1000.0
}

/// Clutter animation modes used here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    EaseOutQuad,
    EaseOutExpo,
}

impl Curve {
    pub fn apply(self, t: f64) -> f64 {
        match self {
            Curve::EaseOutQuad => ease_out_quad(t),
            Curve::EaseOutExpo => ease_out_expo(t),
        }
    }
}

/// Clutter's `EASE_OUT_QUAD`.
pub fn ease_out_quad(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Clutter's `EASE_OUT_EXPO`: `1 - 2^(-10 t)`, exactly 1 at the end.
pub fn ease_out_expo(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 {
        1.0
    } else {
        1.0 - 2f64.powf(-10.0 * t)
    }
}

/// Linear progress of `elapsed` through `duration`; a zero duration is
/// already done.
pub fn progress(elapsed: f64, duration: f64) -> f64 {
    if duration <= 0.0 {
        1.0
    } else {
        (elapsed / duration).clamp(0.0, 1.0)
    }
}

/// One eased value. Retargeting starts from wherever the value is
/// drawn now, so an interrupted effect never jumps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    from: f64,
    to: f64,
    elapsed: f64,
    duration: f64,
    curve: Curve,
}

impl Tween {
    /// A value at rest.
    pub fn at(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            elapsed: 0.0,
            duration: 0.0,
            curve: Curve::EaseOutQuad,
        }
    }

    pub fn new(from: f64, to: f64, duration: f64, curve: Curve) -> Self {
        Self {
            from,
            to,
            elapsed: 0.0,
            duration,
            curve,
        }
    }

    pub fn value(&self) -> f64 {
        let p = progress(self.elapsed, self.duration);
        if p >= 1.0 {
            self.to
        } else {
            self.from + (self.to - self.from) * self.curve.apply(p)
        }
    }

    pub fn target(&self) -> f64 {
        self.to
    }

    pub fn done(&self) -> bool {
        progress(self.elapsed, self.duration) >= 1.0
    }

    /// Head for `to` from the current value.
    pub fn retarget(&mut self, to: f64, duration: f64, curve: Curve) {
        *self = Self::new(self.value(), to, duration, curve);
    }

    pub fn step(&mut self, dt: f64) {
        self.elapsed += dt.max(0.0);
    }

    pub fn finish(&mut self) {
        *self = Self::at(self.to);
    }
}

/// GNOME's animation window types: anything with a transient parent
/// animates as a dialog (`_getAnimationWindowType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowType {
    Normal,
    Dialog,
}

impl WindowType {
    fn name(self) -> &'static str {
        match self {
            WindowType::Normal => "normal",
            WindowType::Dialog => "dialog",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Open,
    Close,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Open => "open",
            Phase::Close => "close",
        }
    }
}

/// Where an effect heads: opacity (the fade part), scale about a pivot
/// given as a fraction of the window (the motion part).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub opacity: f64,
    pub scale: (f64, f64),
    pub pivot: (f64, f64),
}

/// A window's drawn transform this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visual {
    pub opacity: f64,
    pub scale: (f64, f64),
    /// Pivot as a fraction of the window geometry.
    pub pivot: (f64, f64),
    /// Logical translation (keeps a pivot change continuous).
    pub shift: (f64, f64),
}

impl Visual {
    pub const IDENTITY: Visual = Visual {
        opacity: 1.0,
        scale: (1.0, 1.0),
        pivot: (0.0, 0.0),
        shift: (0.0, 0.0),
    };

    /// The logical map `x -> k x + b` per axis this visual applies to a
    /// window drawn at `geometry`.
    pub fn affine(&self, geometry: Rectangle<i32, Logical>) -> Affine {
        let p = (
            f64::from(geometry.loc.x) + self.pivot.0 * f64::from(geometry.size.w),
            f64::from(geometry.loc.y) + self.pivot.1 * f64::from(geometry.size.h),
        );
        Affine {
            k: self.scale,
            b: (
                p.0 * (1.0 - self.scale.0) + self.shift.0,
                p.1 * (1.0 - self.scale.1) + self.shift.1,
            ),
        }
    }
}

/// Per-axis scale and offset: `x -> k x + b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub k: (f64, f64),
    pub b: (f64, f64),
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        k: (1.0, 1.0),
        b: (0.0, 0.0),
    };

    /// Scaling about `about` by `k`.
    pub fn about(about: (f64, f64), k: (f64, f64)) -> Self {
        Self {
            k,
            b: (about.0 * (1.0 - k.0), about.1 * (1.0 - k.1)),
        }
    }

    /// `self` applied after `inner`.
    pub fn then(self, inner: Affine) -> Affine {
        Affine {
            k: (self.k.0 * inner.k.0, self.k.1 * inner.k.1),
            b: (
                self.k.0 * inner.b.0 + self.b.0,
                self.k.1 * inner.b.1 + self.b.1,
            ),
        }
    }

    /// The same map in an output's physical pixels, given the output's
    /// logical origin and scale.
    pub fn to_physical(self, offset: (f64, f64), scale: f64) -> Affine {
        Affine {
            k: self.k,
            b: (
                (self.b.0 - offset.0 * (1.0 - self.k.0)) * scale,
                (self.b.1 - offset.1 * (1.0 - self.k.1)) * scale,
            ),
        }
    }

    /// How to draw a surface tree normally placed at physical `origin`
    /// under this (physical) map with Smithay's rescale element: render
    /// it at the returned location, then rescale about `origin` by `k`.
    /// `None` when the map collapses it to nothing.
    pub fn placement(self, origin: (i32, i32)) -> Option<(i32, i32)> {
        if self.k.0 < 1e-4 || self.k.1 < 1e-4 {
            return None;
        }
        let shift = |c: i32, k: f64, b: f64| {
            let c = f64::from(c);
            c + ((b - c * (1.0 - k)) / k).round()
        };
        Some((
            shift(origin.0, self.k.0, self.b.0) as i32,
            shift(origin.1, self.k.1, self.b.1) as i32,
        ))
    }
}

/// One window's running effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Effect {
    pub phase: Phase,
    pub kind: WindowType,
    opacity: Tween,
    scale_x: Tween,
    scale_y: Tween,
    shift_x: Tween,
    shift_y: Tween,
    pivot: (f64, f64),
    duration: f64,
    started_at: f64,
    frames: u32,
}

impl Effect {
    fn at_rest(phase: Phase, kind: WindowType, now: f64) -> Self {
        Self {
            phase,
            kind,
            opacity: Tween::at(1.0),
            scale_x: Tween::at(1.0),
            scale_y: Tween::at(1.0),
            shift_x: Tween::at(0.0),
            shift_y: Tween::at(0.0),
            pivot: (0.0, 0.0),
            duration: 0.0,
            started_at: now,
            frames: 0,
        }
    }

    /// GNOME's map effect, or `None` when this policy maps instantly.
    pub fn open(kind: WindowType, policy: MotionPolicy, now: f64) -> Option<Self> {
        if !policy.allows_fades() {
            return None;
        }
        let motion = policy.allows_motion();
        let mut effect = Self::at_rest(Phase::Open, kind, now);
        match kind {
            WindowType::Normal => {
                // Bottom-centre pivot, from 0.01 x 0.05 and transparent.
                let (sx, sy) = if motion { (0.01, 0.05) } else { (1.0, 1.0) };
                let d = adjusted(policy, SHOW_WINDOW);
                let c = Curve::EaseOutExpo;
                effect.pivot = (0.5, 1.0);
                effect.duration = d;
                effect.opacity = Tween::new(0.0, 1.0, d, c);
                effect.scale_x = Tween::new(sx, 1.0, d, c);
                effect.scale_y = Tween::new(sy, 1.0, d, c);
            }
            // GNOME fades a dialog in only along with its motion.
            WindowType::Dialog if !motion => return None,
            WindowType::Dialog => {
                let d = adjusted(policy, DIALOG_SHOW_WINDOW);
                let c = Curve::EaseOutQuad;
                effect.pivot = (0.5, 0.5);
                effect.duration = d;
                effect.opacity = Tween::new(0.0, 1.0, d, c);
                effect.scale_y = Tween::new(0.0, 1.0, d, c);
            }
        }
        Some(effect)
    }

    /// GNOME's destroy target for `kind` under `policy`, and its
    /// adjusted duration.
    pub fn close_target(kind: WindowType, policy: MotionPolicy) -> (Target, f64) {
        let full = policy.allows_motion();
        match kind {
            WindowType::Normal => (
                Target {
                    opacity: 0.0,
                    scale: if full { (0.8, 0.8) } else { (1.0, 1.0) },
                    pivot: (0.5, 0.5),
                },
                adjusted(policy, DESTROY_WINDOW),
            ),
            // A dialog either folds up or, without motion, fades.
            WindowType::Dialog => (
                Target {
                    opacity: if full { 1.0 } else { 0.0 },
                    scale: if full { (1.0, 0.0) } else { (1.0, 1.0) },
                    pivot: (0.5, 0.5),
                },
                adjusted(policy, DIALOG_DESTROY_WINDOW),
            ),
        }
    }

    /// Head for `target` from the current visual state. A pivot change
    /// is absorbed by a translation easing to zero, so nothing jumps.
    pub fn retarget(
        &mut self,
        phase: Phase,
        target: Target,
        duration: f64,
        curve: Curve,
        size: Size<i32, Logical>,
        now: f64,
    ) {
        let (sx, sy) = (self.scale_x.value(), self.scale_y.value());
        let jump = |old: f64, new: f64, len: i32, s: f64| (old - new) * f64::from(len) * (1.0 - s);
        let shift = (
            self.shift_x.value() + jump(self.pivot.0, target.pivot.0, size.w, sx),
            self.shift_y.value() + jump(self.pivot.1, target.pivot.1, size.h, sy),
        );
        self.shift_x = Tween::new(shift.0, 0.0, duration, curve);
        self.shift_y = Tween::new(shift.1, 0.0, duration, curve);
        self.pivot = target.pivot;
        self.opacity.retarget(target.opacity, duration, curve);
        self.scale_x.retarget(target.scale.0, duration, curve);
        self.scale_y.retarget(target.scale.1, duration, curve);
        self.phase = phase;
        self.duration = duration;
        self.started_at = now;
        self.frames = 0;
    }

    pub fn visual(&self) -> Visual {
        Visual {
            opacity: self.opacity.value(),
            scale: (self.scale_x.value(), self.scale_y.value()),
            pivot: self.pivot,
            shift: (self.shift_x.value(), self.shift_y.value()),
        }
    }

    fn tweens(&mut self) -> [&mut Tween; 5] {
        [
            &mut self.opacity,
            &mut self.scale_x,
            &mut self.scale_y,
            &mut self.shift_x,
            &mut self.shift_y,
        ]
    }

    pub fn done(&self) -> bool {
        [
            self.opacity,
            self.scale_x,
            self.scale_y,
            self.shift_x,
            self.shift_y,
        ]
        .iter()
        .all(Tween::done)
    }

    /// Advance; returns whether still running.
    fn step(&mut self, dt: f64) -> bool {
        for tween in self.tweens() {
            tween.step(dt);
        }
        if self.done() {
            false
        } else {
            self.frames += 1;
            true
        }
    }

    fn finish(&mut self) {
        for tween in self.tweens() {
            tween.finish();
        }
    }
}

/// One surface of a closing window's last frame, relative to the
/// window's root surface origin.
#[derive(Debug, Clone)]
pub struct SnapshotSurface {
    pub id: Id,
    pub texture: GlesTexture,
    pub offset: Point<i32, Logical>,
    pub src: Rectangle<f64, Logical>,
    pub size: Size<i32, Logical>,
    pub buffer_scale: i32,
    pub transform: Transform,
}

/// A window's last frame: its surface tree's textures, front to back
/// (Smithay's own surface-tree order).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub surfaces: Vec<SnapshotSurface>,
    /// The window geometry's offset inside the root surface.
    pub geometry_loc: Point<i32, Logical>,
}

/// Snapshots taken as toplevels lose their role or surface, waiting
/// for the window manager's next reconcile to claim them.
#[derive(Debug, Default)]
pub struct SnapshotStash {
    context: Option<ContextId<GlesTexture>>,
    taken: HashMap<WlSurface, Snapshot>,
}

impl SnapshotStash {
    /// The renderer whose textures snapshots hold (set every frame).
    pub fn set_context(&mut self, context: ContextId<GlesTexture>) {
        self.context = Some(context);
    }

    /// Keep `surface`'s current textures unless a snapshot is already
    /// held (the first capture is the last drawn frame).
    pub fn capture(&mut self, surface: &WlSurface) {
        if self.taken.contains_key(surface) {
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let snapshot = capture_tree(surface, context);
        if !snapshot.surfaces.is_empty() {
            self.taken.insert(surface.clone(), snapshot);
        }
    }

    pub fn take(&mut self, surface: &WlSurface) -> Option<Snapshot> {
        self.taken.remove(surface)
    }

    /// Drop unclaimed snapshots (roles that never entered the model).
    pub fn clear(&mut self) {
        self.taken.clear();
    }
}

fn capture_tree(surface: &WlSurface, context: ContextId<GlesTexture>) -> Snapshot {
    use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
    use smithay::wayland::compositor::{
        with_states, with_surface_tree_downward, SurfaceData, TraversalAction,
    };
    let mut surfaces = Vec::new();
    let keep = |states: &SurfaceData, location: Point<i32, Logical>| {
        let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
        let data = data.lock().ok()?;
        let (view, texture) = (data.view()?, data.texture(context.clone())?);
        Some(SnapshotSurface {
            id: Id::new(),
            texture: texture.clone(),
            offset: location + view.offset,
            src: view.src,
            size: view.dst,
            buffer_scale: data.buffer_scale(),
            transform: data.buffer_transform(),
        })
    };
    with_surface_tree_downward(
        surface,
        Point::<i32, Logical>::from((0, 0)),
        |_, states, location| {
            let data = states.data_map.get::<RendererSurfaceStateUserData>();
            match data.and_then(|d| d.lock().ok().and_then(|d| d.view())) {
                Some(view) => TraversalAction::DoChildren(*location + view.offset),
                None => TraversalAction::SkipChildren,
            }
        },
        |_, states, location| surfaces.extend(keep(states, *location)),
        |_, _, _| true,
    );
    // A surface being destroyed has already left its own tree (Smithay
    // empties it before the destruction hooks run): keep the root alone.
    if surfaces.is_empty() {
        surfaces.extend(with_states(surface, |states| keep(states, (0, 0).into())));
    }
    let geometry_loc = with_states(surface, |states| {
        states
            .cached_state
            .get::<smithay::wayland::shell::xdg::SurfaceCachedState>()
            .current()
            .geometry
            .map(|g| g.loc)
            .unwrap_or_default()
    });
    Snapshot {
        surfaces,
        geometry_loc,
    }
}

/// A window that has left the model and plays its close effect from
/// its last frame.
#[derive(Debug, Clone)]
pub struct Closing {
    pub id: u64,
    pub workspace: u32,
    pub sticky: bool,
    /// The window it was stacked directly above (`None`: bottom).
    pub above: Option<u64>,
    /// Window geometry where it was last drawn.
    pub geometry: Rectangle<i32, Logical>,
    pub snapshot: Snapshot,
    effect: Effect,
}

impl Closing {
    pub fn visual(&self) -> Visual {
        self.effect.visual()
    }
}

/// What the manager knows about a window as it leaves.
#[derive(Debug, Clone)]
pub struct Leaving {
    pub id: u64,
    pub kind: WindowType,
    pub workspace: u32,
    pub sticky: bool,
    pub above: Option<u64>,
    pub geometry: Rectangle<i32, Logical>,
    pub snapshot: Option<Snapshot>,
}

/// A finished (or skipped) effect, for the state file.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub id: u64,
    pub what: &'static str,
    pub kind: &'static str,
    /// GNOME's duration for it, ms; 0 when it ran instantly.
    pub duration_ms: f64,
    /// Wall time from start to settle, ms.
    pub settled_ms: f64,
    /// Frames drawn mid-effect.
    pub frames: u32,
}

/// A dimmed window's colour multiplier this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Brightness {
    pub value: f32,
    /// Changes whenever `value` does (damage tracking).
    pub generation: u64,
    /// Not moving: only then may it hide what is beneath it.
    pub settled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Dim {
    brightness: Tween,
    started_at: f64,
    frames: u32,
    /// Fresh per brightness change, so damage tracking redraws it.
    id_generation: u64,
    /// Retargeted and not yet logged as settled.
    unlogged: bool,
}

impl Dim {
    fn retarget(&mut self, to: f64, duration: f64, now: f64) {
        self.brightness.retarget(to, duration, Curve::EaseOutQuad);
        self.started_at = now;
        self.frames = 0;
        self.id_generation += 1;
        self.unlogged = true;
    }
}

/// Every window's effects: opening and other live retargets, closing
/// snapshots, and dimmed parents.
#[derive(Debug)]
pub struct WindowEffects {
    motion: MotionPolicy,
    animate: bool,
    /// Seconds since creation, advanced by [`step`](Self::step).
    clock: f64,
    /// Mapped windows awaiting their first buffer.
    pending: HashSet<u64>,
    live: HashMap<u64, Effect>,
    closing: Vec<Closing>,
    dims: HashMap<u64, Dim>,
    log: VecDeque<Finished>,
}

impl Default for WindowEffects {
    fn default() -> Self {
        Self {
            motion: MotionPolicy::default(),
            animate: true,
            clock: 0.0,
            pending: HashSet::new(),
            live: HashMap::new(),
            closing: Vec::new(),
            dims: HashMap::new(),
            log: VecDeque::new(),
        }
    }
}

impl WindowEffects {
    /// The motion policy, and whether new effects may animate now
    /// (GNOME's `_shouldAnimate`: not while the overview is open or a
    /// workspace swipe runs). Turning motion off settles everything.
    pub fn set_policy(&mut self, motion: MotionPolicy, animate: bool) {
        self.motion = motion;
        self.animate = animate;
        if !motion.allows_fades() {
            self.settle_all();
        }
    }

    fn animating_allowed(&self) -> bool {
        self.animate && self.motion.allows_fades()
    }

    /// A window entered the model; its effect starts with its first
    /// buffer ([`start_ready`](Self::start_ready)).
    pub fn mapped(&mut self, id: u64) {
        self.pending.insert(id);
    }

    /// Windows mapped but not yet shown.
    pub fn pending(&self) -> Vec<u64> {
        self.pending.iter().copied().collect()
    }

    /// `id` is gone before it showed.
    pub fn forget(&mut self, id: u64) {
        self.pending.remove(&id);
        self.live.remove(&id);
    }

    /// `id` drew its first buffer: start GNOME's map effect.
    pub fn shown(&mut self, id: u64, kind: WindowType) {
        if !self.pending.remove(&id) {
            return;
        }
        let effect = self
            .animating_allowed()
            .then(|| Effect::open(kind, self.motion, self.clock))
            .flatten();
        match effect {
            Some(effect) => {
                self.live.insert(id, effect);
            }
            None => self.record(id, Phase::Open, kind, 0.0, 0.0, 0),
        }
    }

    /// A window left the model: play GNOME's destroy effect from its
    /// last frame, continuing whatever it was doing.
    pub fn closed(&mut self, leaving: Leaving) {
        self.pending.remove(&leaving.id);
        let current = self.live.remove(&leaving.id);
        let snapshot = leaving.snapshot.filter(|s| !s.surfaces.is_empty());
        let (Some(snapshot), true) = (snapshot, self.animating_allowed()) else {
            self.record(leaving.id, Phase::Close, leaving.kind, 0.0, 0.0, 0);
            return;
        };
        let mut effect =
            current.unwrap_or_else(|| Effect::at_rest(Phase::Close, leaving.kind, self.clock));
        effect.kind = leaving.kind;
        let (target, duration) = Effect::close_target(leaving.kind, self.motion);
        effect.retarget(
            Phase::Close,
            target,
            duration,
            Curve::EaseOutQuad,
            leaving.geometry.size,
            self.clock,
        );
        self.closing.push(Closing {
            id: leaving.id,
            workspace: leaving.workspace,
            sticky: leaving.sticky,
            above: leaving.above,
            geometry: leaving.geometry,
            snapshot,
            effect,
        });
    }

    /// Retarget a live window's effect (another family's action, e.g.
    /// minimize, interrupting an open) from its current visual state.
    pub fn retarget(
        &mut self,
        id: u64,
        kind: WindowType,
        target: Target,
        duration: f64,
        curve: Curve,
        size: Size<i32, Logical>,
    ) {
        let now = self.clock;
        self.pending.remove(&id);
        let effect = self
            .live
            .entry(id)
            .or_insert_with(|| Effect::at_rest(Phase::Open, kind, now));
        let phase = effect.phase;
        effect.retarget(phase, target, duration, curve, size, now);
    }

    /// Parents with an attached modal dialog dim over 500 ms; the rest
    /// undim over 250 ms (GNOME's `_checkDimming`).
    pub fn set_dimmed(&mut self, parents: &HashSet<u64>) {
        // Brightness is not motion: it animates under reduced motion.
        let animate = self.animate && self.motion.allows_fades();
        let now = self.clock;
        for id in parents {
            let dim = self.dims.entry(*id).or_insert(Dim {
                brightness: Tween::at(1.0),
                started_at: now,
                frames: 0,
                id_generation: 0,
                unlogged: false,
            });
            if dim.brightness.target() != DIMMED {
                dim.retarget(
                    DIMMED,
                    if animate {
                        adjusted(self.motion, DIM)
                    } else {
                        0.0
                    },
                    now,
                );
            }
        }
        for (id, dim) in self.dims.iter_mut() {
            if !parents.contains(id) && dim.brightness.target() != 1.0 {
                dim.retarget(
                    1.0,
                    if animate {
                        adjusted(self.motion, UNDIM)
                    } else {
                        0.0
                    },
                    now,
                );
            }
        }
    }

    /// Advance every effect by `dt` seconds; returns whether any still
    /// runs (frames must keep coming).
    pub fn step(&mut self, dt: f64) -> bool {
        let dt = dt.max(0.0);
        self.clock += dt;
        let mut finished = Vec::new();
        self.live.retain(|id, effect| {
            let running = effect.step(dt);
            if !running {
                finished.push((*id, *effect));
            }
            running
        });
        self.closing.retain_mut(|closing| {
            let running = closing.effect.step(dt);
            if !running {
                finished.push((closing.id, closing.effect));
            }
            running
        });
        for (id, effect) in finished {
            self.record_effect(id, &effect);
        }
        let mut dims_done = Vec::new();
        for (id, dim) in self.dims.iter_mut() {
            if dim.brightness.done() && !dim.unlogged {
                continue;
            }
            dim.brightness.step(dt);
            dim.id_generation += 1;
            if !dim.brightness.done() {
                dim.frames += 1;
            } else if dim.unlogged {
                dim.unlogged = false;
                dims_done.push((*id, *dim));
            }
        }
        for (id, dim) in dims_done {
            let dimmed = dim.brightness.target() != 1.0;
            let duration = dim.brightness.duration;
            self.push_log(Finished {
                id,
                what: if dimmed { "dim" } else { "undim" },
                kind: "parent",
                duration_ms: duration * 1000.0,
                settled_ms: (self.clock - dim.started_at) * 1000.0,
                frames: dim.frames,
            });
        }
        self.dims
            .retain(|_, dim| !(dim.brightness.done() && dim.brightness.target() == 1.0));
        self.running()
    }

    pub fn running(&self) -> bool {
        !self.live.is_empty()
            || !self.closing.is_empty()
            || self.dims.values().any(|d| !d.brightness.done())
    }

    /// Finish everything in flight now.
    pub fn settle_all(&mut self) {
        let live: Vec<_> = self.live.drain().collect();
        for (id, mut effect) in live {
            effect.finish();
            self.record_effect(id, &effect);
        }
        let closing = std::mem::take(&mut self.closing);
        for mut closing in closing {
            closing.effect.finish();
            self.record_effect(closing.id, &closing.effect);
        }
        for dim in self.dims.values_mut().filter(|d| !d.brightness.done()) {
            dim.brightness.finish();
            dim.id_generation += 1;
        }
        self.dims.retain(|_, dim| dim.brightness.target() != 1.0);
    }

    fn record_effect(&mut self, id: u64, effect: &Effect) {
        let settled = (self.clock - effect.started_at) * 1000.0;
        self.record(
            id,
            effect.phase,
            effect.kind,
            effect.duration * 1000.0,
            settled,
            effect.frames,
        );
    }

    fn record(
        &mut self,
        id: u64,
        phase: Phase,
        kind: WindowType,
        duration_ms: f64,
        settled_ms: f64,
        frames: u32,
    ) {
        self.push_log(Finished {
            id,
            what: phase.name(),
            kind: kind.name(),
            duration_ms,
            settled_ms,
            frames,
        });
    }

    fn push_log(&mut self, entry: Finished) {
        if self.log.len() == LOG_LEN {
            self.log.pop_front();
        }
        self.log.push_back(entry);
    }

    /// Live window `id`'s drawn transform, if it has an effect.
    pub fn visual(&self, id: u64) -> Option<Visual> {
        self.live.get(&id).map(Effect::visual)
    }

    /// Whether `id` is mapped but has not shown yet (drawn hidden until
    /// its open effect starts, so its first frame is the effect's).
    pub fn is_pending(&self, id: u64) -> bool {
        self.pending.contains(&id)
    }

    /// Window `id`'s colour multiplier while dimmed, and a generation
    /// that changes whenever the multiplier does.
    pub fn brightness(&self, id: u64) -> Option<Brightness> {
        let dim = self.dims.get(&id)?;
        let value = dim.brightness.value();
        (value < 1.0).then_some(Brightness {
            value: value as f32,
            generation: dim.id_generation,
            settled: dim.brightness.done(),
        })
    }

    pub fn closing(&self) -> &[Closing] {
        &self.closing
    }

    /// In-flight and recently finished effects for the state file.
    pub fn state_json(&self) -> serde_json::Value {
        let effect = |id: &u64, e: &Effect| {
            let v = e.visual();
            serde_json::json!({
                "id": id,
                "what": e.phase.name(),
                "kind": e.kind.name(),
                "duration_ms": e.duration * 1000.0,
                "elapsed_ms": (self.clock - e.started_at) * 1000.0,
                "opacity": v.opacity,
                "scale": [v.scale.0, v.scale.1],
            })
        };
        let mut active: Vec<_> = self.live.iter().map(|(id, e)| effect(id, e)).collect();
        active.extend(self.closing.iter().map(|c| effect(&c.id, &c.effect)));
        let dims: Vec<_> = self
            .dims
            .iter()
            .map(|(id, d)| {
                serde_json::json!({
                    "id": id,
                    "brightness": d.brightness.value(),
                    "target": d.brightness.target(),
                })
            })
            .collect();
        let finished: Vec<_> = self
            .log
            .iter()
            .map(|f| {
                serde_json::json!({
                    "id": f.id,
                    "what": f.what,
                    "kind": f.kind,
                    "duration_ms": f.duration_ms,
                    "settled_ms": f.settled_ms,
                    "frames": f.frames,
                })
            })
            .collect();
        serde_json::json!({
            "active": active,
            "dimmed": dims,
            "finished": finished,
        })
    }
}

/// A surface tree element as the scene draws it (runtime.rs).
pub type SurfaceElement = smithay::backend::renderer::element::utils::RescaleRenderElement<
    smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement<
        smithay::backend::renderer::gles::GlesRenderer,
    >,
>;

/// A closing window's kept texture, transformed.
pub type SnapshotElement = smithay::backend::renderer::element::utils::RescaleRenderElement<
    smithay::backend::renderer::element::texture::TextureRenderElement<GlesTexture>,
>;

smithay::backend::renderer::element::render_elements! {
    /// Everything the window scene draws.
    pub SceneElement<=smithay::backend::renderer::gles::GlesRenderer>;
    Surface=SurfaceElement,
    Dimmed=DimElement,
    Snapshot=SnapshotElement,
}

/// A dimmed parent's surface: drawn through a texture shader that
/// multiplies colour, as Clutter's brightness effect does. Its id
/// follows the dim generation, so damage tracking redraws it while
/// the brightness moves and leaves it alone once it settles.
#[derive(Debug)]
pub struct DimElement {
    inner: SurfaceElement,
    id: Id,
    brightness: f32,
    /// Occlusion culling may hide what is under it (#531) only once the
    /// dim has settled.
    opaque: bool,
    program: smithay::backend::renderer::gles::GlesTexProgram,
}

/// Premultiplied colour times `brightness`; Smithay's texture shader
/// contract otherwise (`//_DEFINES_`, `EXTERNAL`, `NO_ALPHA`).
const DIM_SHADER: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
uniform float brightness;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif
    color = vec4(color.rgb * brightness, color.a) * alpha;
    gl_FragColor = color;
}
"#;

thread_local! {
    static DIM_PROGRAM: std::cell::RefCell<
        Option<(ContextId<GlesTexture>, smithay::backend::renderer::gles::GlesTexProgram)>,
    > = const { std::cell::RefCell::new(None) };
    /// Dim element ids per (surface element, generation).
    static DIM_IDS: std::cell::RefCell<HashMap<Id, (u64, Id)>> =
        std::cell::RefCell::new(HashMap::new());
}

impl DimElement {
    /// `inner` dimmed to `brightness`, or as it is when the shader
    /// cannot compile.
    pub fn wrap(
        renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
        inner: SurfaceElement,
        dim: Brightness,
    ) -> SceneElement {
        let Brightness {
            value: brightness,
            generation,
            settled,
        } = dim;
        use smithay::backend::renderer::element::Element;
        use smithay::backend::renderer::gles::{UniformName, UniformType};
        use smithay::backend::renderer::Renderer;
        let context = renderer.context_id();
        let program = DIM_PROGRAM.with(|cell| {
            let mut cell = cell.borrow_mut();
            if let Some((ctx, program)) = cell.as_ref() {
                if *ctx == context {
                    return Some(program.clone());
                }
            }
            let program = renderer
                .compile_custom_texture_shader(
                    DIM_SHADER,
                    &[UniformName::new("brightness", UniformType::_1f)],
                )
                .map_err(|e| eprintln!("tuna-compositor: dim shader: {e}"))
                .ok()?;
            *cell = Some((context, program.clone()));
            Some(program)
        });
        let Some(program) = program else {
            return SceneElement::Surface(inner);
        };
        let id = DIM_IDS.with(|cell| {
            let mut ids = cell.borrow_mut();
            if ids.len() > 512 {
                ids.clear();
            }
            let entry = ids
                .entry(inner.id().clone())
                .or_insert_with(|| (generation, Id::new()));
            if entry.0 != generation {
                *entry = (generation, Id::new());
            }
            entry.1.clone()
        });
        SceneElement::Dimmed(Self {
            inner,
            id,
            brightness,
            opaque: settled,
            program,
        })
    }
}

impl smithay::backend::renderer::element::Element for DimElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> smithay::backend::renderer::utils::CommitCounter {
        self.inner.current_commit()
    }
    fn location(&self, scale: smithay::utils::Scale<f64>) -> Point<i32, smithay::utils::Physical> {
        self.inner.location(scale)
    }
    fn src(&self) -> Rectangle<f64, smithay::utils::Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(
        &self,
        scale: smithay::utils::Scale<f64>,
    ) -> Rectangle<i32, smithay::utils::Physical> {
        self.inner.geometry(scale)
    }
    fn damage_since(
        &self,
        scale: smithay::utils::Scale<f64>,
        commit: Option<smithay::backend::renderer::utils::CommitCounter>,
    ) -> smithay::backend::renderer::utils::DamageSet<i32, smithay::utils::Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(
        &self,
        scale: smithay::utils::Scale<f64>,
    ) -> smithay::backend::renderer::utils::OpaqueRegions<i32, smithay::utils::Physical> {
        if self.opaque {
            self.inner.opaque_regions(scale)
        } else {
            Default::default()
        }
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> smithay::backend::renderer::element::Kind {
        self.inner.kind()
    }
}

impl
    smithay::backend::renderer::element::RenderElement<
        smithay::backend::renderer::gles::GlesRenderer,
    > for DimElement
{
    fn draw(
        &self,
        frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
        src: Rectangle<f64, smithay::utils::Buffer>,
        dst: Rectangle<i32, smithay::utils::Physical>,
        damage: &[Rectangle<i32, smithay::utils::Physical>],
        opaque_regions: &[Rectangle<i32, smithay::utils::Physical>],
    ) -> Result<(), smithay::backend::renderer::gles::GlesError> {
        use smithay::backend::renderer::element::RenderElement;
        use smithay::backend::renderer::gles::Uniform;
        frame.override_default_tex_program(
            self.program.clone(),
            vec![Uniform::new("brightness", self.brightness)],
        );
        let drawn = RenderElement::<smithay::backend::renderer::gles::GlesRenderer>::draw(
            &self.inner,
            frame,
            src,
            dst,
            damage,
            opaque_regions,
        );
        frame.clear_tex_program_override();
        drawn
    }
}

/// One output's view for the helpers below: its logical origin and
/// physical pixels per logical pixel.
#[derive(Debug, Clone, Copy)]
pub struct OutputView {
    pub offset: (i32, i32),
    pub scale: f64,
}

impl OutputView {
    fn physical(&self, p: Point<f64, Logical>) -> (i32, i32) {
        (
            ((p.x - f64::from(self.offset.0)) * self.scale).round() as i32,
            ((p.y - f64::from(self.offset.1)) * self.scale).round() as i32,
        )
    }
}

/// A closing window's last frame under its close effect, front to
/// back, in one output's pixels, moved `dx` and faded by `alpha` with a
/// running workspace switch.
pub fn closing_elements(
    renderer: &mut smithay::backend::renderer::gles::GlesRenderer,
    closing: &Closing,
    view: OutputView,
    dx: i32,
    alpha: f32,
) -> Vec<SceneElement> {
    use smithay::backend::renderer::element::texture::TextureRenderElement;
    use smithay::backend::renderer::element::utils::RescaleRenderElement;
    use smithay::backend::renderer::element::Kind;
    use smithay::backend::renderer::Renderer;
    let visual = closing.visual();
    let mut geometry = closing.geometry;
    geometry.loc.x += dx;
    let map = visual.affine(geometry).to_physical(
        (f64::from(view.offset.0), f64::from(view.offset.1)),
        view.scale,
    );
    let root = (geometry.loc - closing.snapshot.geometry_loc).to_f64();
    let origin = view.physical(root);
    let Some(at) = map.placement(origin) else {
        return Vec::new();
    };
    let context = renderer.context_id();
    closing
        .snapshot
        .surfaces
        .iter()
        .map(|surface| {
            let location: Point<f64, smithay::utils::Physical> = (
                f64::from(at.0) + f64::from(surface.offset.x) * view.scale,
                f64::from(at.1) + f64::from(surface.offset.y) * view.scale,
            )
                .into();
            let element = TextureRenderElement::from_static_texture(
                surface.id.clone(),
                context.clone(),
                location,
                surface.texture.clone(),
                surface.buffer_scale,
                surface.transform,
                Some(visual.opacity as f32 * alpha),
                Some(surface.src),
                Some(surface.size),
                None,
                Kind::Unspecified,
            );
            SceneElement::Snapshot(RescaleRenderElement::from_element(
                element,
                origin.into(),
                smithay::utils::Scale::from(map.k),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn easing_endpoints_and_midpoints_match_clutter() {
        assert_eq!(ease_out_quad(0.0), 0.0);
        assert_eq!(ease_out_quad(0.5), 0.75);
        assert_eq!(ease_out_quad(1.0), 1.0);
        assert_eq!(ease_out_quad(2.0), 1.0);
        assert_eq!(ease_out_expo(0.0), 0.0);
        assert!(close(ease_out_expo(0.5), 1.0 - 2f64.powi(-5)));
        assert_eq!(ease_out_expo(1.0), 1.0);
        assert_eq!(progress(0.075, 0.150), 0.5);
        assert_eq!(progress(1.0, 0.0), 1.0);
    }

    #[test]
    fn tween_retargets_from_its_drawn_value() {
        let mut t = Tween::new(0.0, 1.0, 0.2, Curve::EaseOutQuad);
        t.step(0.1);
        assert_eq!(t.value(), 0.75);
        t.retarget(0.0, 0.2, Curve::EaseOutQuad);
        assert_eq!(t.value(), 0.75);
        t.step(0.2);
        assert!(t.done());
        assert_eq!(t.value(), 0.0);
    }

    #[test]
    fn normal_window_opens_from_bottom_centre_in_150_ms() {
        let mut e = Effect::open(WindowType::Normal, MotionPolicy::default(), 0.0).unwrap();
        let v = e.visual();
        assert_eq!(
            (v.opacity, v.scale, v.pivot),
            (0.0, (0.01, 0.05), (0.5, 1.0))
        );
        assert!(e.step(0.075));
        let mid = e.visual();
        assert!(close(mid.opacity, ease_out_expo(0.5)));
        assert!(mid.scale.0 > 0.9 && mid.scale.0 < 1.0);
        assert!(!e.step(0.075));
        assert_eq!(e.visual().scale, (1.0, 1.0));
        assert_eq!(e.visual().opacity, 1.0);
        // The pivot sits at the bottom centre: the bottom edge stays put.
        let geo = Rectangle::new((100, 50).into(), (400, 300).into());
        let a = v.affine(geo);
        assert!(close(a.k.1 * 350.0 + a.b.1, 350.0));
        assert!(close(a.k.0 * 300.0 + a.b.0, 300.0));
    }

    #[test]
    fn reduced_motion_keeps_fades_and_off_is_instant() {
        let e = Effect::open(
            WindowType::Normal,
            MotionPolicy::new(MotionLevel::FadeOnly, 1.0),
            0.0,
        )
        .unwrap();
        assert_eq!(e.visual().scale, (1.0, 1.0));
        assert_eq!(e.visual().opacity, 0.0);
        assert!(Effect::open(
            WindowType::Normal,
            MotionPolicy::new(MotionLevel::Off, 1.0),
            0.0
        )
        .is_none());
        // GNOME fades dialogs in only with their motion.
        assert!(Effect::open(
            WindowType::Dialog,
            MotionPolicy::new(MotionLevel::FadeOnly, 1.0),
            0.0
        )
        .is_none());
        let (t, d) = Effect::close_target(
            WindowType::Dialog,
            MotionPolicy::new(MotionLevel::FadeOnly, 1.0),
        );
        assert_eq!(
            (t.opacity, t.scale, d),
            (0.0, (1.0, 1.0), DIALOG_DESTROY_WINDOW)
        );
        let (t, d) = Effect::close_target(
            WindowType::Normal,
            MotionPolicy::new(MotionLevel::FadeOnly, 1.0),
        );
        assert_eq!((t.opacity, t.scale, d), (0.0, (1.0, 1.0), DESTROY_WINDOW));
    }

    #[test]
    fn the_slow_down_factor_stretches_every_duration() {
        let slow = MotionPolicy::new(MotionLevel::Full, 2.0);
        let mut e = Effect::open(WindowType::Normal, slow, 0.0).unwrap();
        assert!(e.step(SHOW_WINDOW));
        assert!(!e.step(SHOW_WINDOW));
        let (_, d) = Effect::close_target(WindowType::Dialog, slow);
        assert!(close(d, 2.0 * DIALOG_DESTROY_WINDOW));
        let mut fx = WindowEffects::default();
        fx.set_policy(slow, true);
        fx.set_dimmed(&[3].into());
        fx.step(DIM);
        assert!(fx.running());
        fx.step(DIM);
        assert_eq!(fx.log.back().unwrap().duration_ms, 1000.0);
    }

    #[test]
    fn dialog_folds_open_and_closed_about_its_centre() {
        let mut e = Effect::open(WindowType::Dialog, MotionPolicy::default(), 0.0).unwrap();
        assert_eq!(e.visual().scale, (1.0, 0.0));
        assert!(!e.step(DIALOG_SHOW_WINDOW));
        let (t, d) = Effect::close_target(WindowType::Dialog, MotionPolicy::default());
        assert_eq!((t.opacity, t.scale, d), (1.0, (1.0, 0.0), 0.1));
    }

    #[test]
    fn close_mid_open_continues_without_a_jump() {
        let geo = Rectangle::new((0, 0).into(), (400, 300).into());
        let mut e = Effect::open(WindowType::Normal, MotionPolicy::default(), 0.0).unwrap();
        e.step(0.03);
        let before = e.visual().affine(geo);
        let opacity = e.visual().opacity;
        let (target, duration) = Effect::close_target(WindowType::Normal, MotionPolicy::default());
        e.retarget(
            Phase::Close,
            target,
            duration,
            Curve::EaseOutQuad,
            geo.size,
            0.03,
        );
        let after = e.visual().affine(geo);
        assert_eq!(e.visual().opacity, opacity);
        assert!(close(before.k.0, after.k.0) && close(before.k.1, after.k.1));
        assert!((before.b.0 - after.b.0).abs() < 1e-6);
        assert!((before.b.1 - after.b.1).abs() < 1e-6);
        e.step(duration);
        let end = e.visual();
        assert_eq!(
            (end.opacity, end.scale, end.shift),
            (0.0, (0.8, 0.8), (0.0, 0.0))
        );
    }

    #[test]
    fn affine_placement_reproduces_the_map() {
        // Scale 0.5 about (100, 100), in physical pixels at scale 2.
        let a = Affine::about((100.0, 100.0), (0.5, 0.5)).to_physical((0.0, 0.0), 2.0);
        let origin = (40, 60);
        let at = a.placement(origin).unwrap();
        // A point drawn at `at + v`, rescaled about `origin`, lands
        // where the map sends `origin + v`.
        for v in [(0.0, 0.0), (300.0, 120.0)] {
            let drawn = (
                f64::from(origin.0) + (f64::from(at.0) + v.0 - f64::from(origin.0)) * 0.5,
                f64::from(origin.1) + (f64::from(at.1) + v.1 - f64::from(origin.1)) * 0.5,
            );
            let mapped = (
                a.k.0 * (f64::from(origin.0) + v.0) + a.b.0,
                a.k.1 * (f64::from(origin.1) + v.1) + a.b.1,
            );
            assert!((drawn.0 - mapped.0).abs() <= 0.5 && (drawn.1 - mapped.1).abs() <= 0.5);
        }
        assert_eq!(Affine::IDENTITY.placement(origin), Some(origin));
        assert_eq!(
            Affine::about((0.0, 0.0), (1.0, 0.0)).placement(origin),
            None
        );
        let strip = Affine::about((10.0, 0.0), (2.0, 1.0));
        let both = Affine::about((0.0, 0.0), (0.5, 0.5)).then(strip);
        assert_eq!(both.k, (1.0, 0.5));
        assert_eq!(both.b, (-5.0, 0.0));
    }

    fn leaving(id: u64, snapshot: bool) -> Leaving {
        Leaving {
            id,
            kind: WindowType::Normal,
            workspace: 1,
            sticky: false,
            above: None,
            geometry: Rectangle::new((0, 0).into(), (400, 300).into()),
            snapshot: snapshot.then(|| Snapshot {
                surfaces: Vec::new(),
                geometry_loc: (0, 0).into(),
            }),
        }
    }

    #[test]
    fn open_waits_for_content_and_logs_its_settle() {
        let mut fx = WindowEffects::default();
        fx.mapped(7);
        assert!(fx.is_pending(7));
        assert!(fx.visual(7).is_none());
        fx.shown(7, WindowType::Normal);
        assert!(fx.visual(7).is_some());
        assert!(fx.step(0.05));
        assert!(fx.step(0.05));
        assert!(!fx.step(0.06));
        let f = fx.log.back().unwrap();
        assert_eq!((f.id, f.what, f.kind, f.frames), (7, "open", "normal", 2));
        assert!(close(f.settled_ms, 160.0) && f.duration_ms == 150.0);
    }

    #[test]
    fn disabled_or_held_effects_are_instant() {
        let mut fx = WindowEffects::default();
        fx.set_policy(MotionPolicy::new(MotionLevel::Off, 1.0), true);
        fx.mapped(1);
        fx.shown(1, WindowType::Normal);
        assert!(fx.visual(1).is_none());
        // The overview open: no effect either, but still logged.
        fx.set_policy(MotionPolicy::default(), false);
        fx.mapped(2);
        fx.shown(2, WindowType::Dialog);
        assert!(fx.visual(2).is_none());
        fx.closed(leaving(2, false));
        assert!(fx.closing().is_empty());
        let whats: Vec<_> = fx
            .log
            .iter()
            .map(|f| (f.id, f.what, f.duration_ms))
            .collect();
        assert_eq!(
            whats,
            [(1, "open", 0.0), (2, "open", 0.0), (2, "close", 0.0)]
        );
        // Turning motion off settles what runs.
        fx.set_policy(MotionPolicy::default(), true);
        fx.mapped(3);
        fx.shown(3, WindowType::Normal);
        fx.set_policy(MotionPolicy::new(MotionLevel::Off, 1.0), true);
        assert!(!fx.running());
    }

    #[test]
    fn closing_needs_a_snapshot_and_ends_with_its_effect() {
        let mut fx = WindowEffects::default();
        fx.closed(leaving(4, false));
        assert!(fx.closing().is_empty());
        // An empty snapshot is no frame to animate.
        fx.closed(leaving(5, true));
        assert!(fx.closing().is_empty());
    }

    #[test]
    fn parent_dims_over_500_ms_and_undims_over_250() {
        let mut fx = WindowEffects::default();
        let parents: HashSet<u64> = [9].into();
        fx.set_dimmed(&parents);
        assert!(fx.brightness(9).is_none());
        assert!(fx.step(0.25));
        let b = fx.brightness(9).unwrap();
        let (half, generation) = (b.value, b.generation);
        assert!(!b.settled);
        assert!((f64::from(half) - (1.0 + DIM_BRIGHTNESS * 0.75)).abs() < 1e-6);
        fx.step(0.25);
        let b = fx.brightness(9).unwrap();
        let (dimmed, settled) = (b.value, b.generation);
        assert!(b.settled);
        assert!((f64::from(dimmed) - DIMMED).abs() < 1e-6);
        assert_ne!(generation, settled);
        // Settled: the generation holds still for damage tracking.
        fx.step(0.1);
        assert_eq!(fx.brightness(9).unwrap().generation, settled);
        fx.set_dimmed(&HashSet::new());
        fx.step(0.125);
        assert!(fx.brightness(9).unwrap().value > dimmed);
        fx.step(0.125);
        assert!(fx.brightness(9).is_none());
        assert!(!fx.running());
        let whats: Vec<_> = fx.log.iter().map(|f| (f.what, f.duration_ms)).collect();
        assert_eq!(whats, [("dim", 500.0), ("undim", 250.0)]);
    }
}
