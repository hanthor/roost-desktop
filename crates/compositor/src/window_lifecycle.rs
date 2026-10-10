//! GNOME 51 window map/destroy effects (windowManager.js, tag 51.0).
//!
//! Import the last shown frame while the surface is alive. Only a destroy
//! effect copies that retained frame into an offscreen texture; destruction
//! never reads protocol state. Steady-state windows incur no offscreen copy.
use crate::size_change::{RectF, Snapshot};
use smithay::backend::renderer::{
    element::surface::WaylandSurfaceRenderElement, gles::GlesRenderer,
};
use smithay::utils::{Logical, Rectangle};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use tuna_shell_control::motion::MotionPolicy;

type Rect = Rectangle<i32, Logical>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Dialog,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Map,
    Destroy,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub scale: (f64, f64),
    pub alpha: f32,
    pub pivot: (f64, f64),
}

pub fn duration(kind: Kind, policy: MotionPolicy) -> Duration {
    Duration::from_secs_f64(
        policy.adjust_ms(match kind {
            Kind::Normal => 150.0,
            Kind::Dialog => 100.0,
        }) / 1000.0,
    )
}
pub fn frame(kind: Kind, effect: Effect, progress: f64, policy: MotionPolicy) -> Frame {
    if !policy.allows_fades() {
        return Frame {
            scale: (1.0, 1.0),
            alpha: if effect == Effect::Map { 1.0 } else { 0.0 },
            pivot: (0.5, 0.5),
        };
    }
    let p = progress.clamp(0.0, 1.0);
    let ease = if kind == Kind::Normal && effect == Effect::Map {
        if p >= 1.0 {
            1.0
        } else if p <= 0.0 {
            0.0
        } else {
            1.0 - 2.0_f64.powf(-10.0 * p)
        }
    } else {
        1.0 - (1.0 - p).powi(2)
    };
    let motion = policy.allows_motion();
    match (kind, effect) {
        (Kind::Normal, Effect::Map) => Frame {
            scale: if motion {
                (0.01 + 0.99 * ease, 0.05 + 0.95 * ease)
            } else {
                (1.0, 1.0)
            },
            alpha: ease as f32,
            pivot: (0.5, 1.0),
        },
        (Kind::Normal, Effect::Destroy) => Frame {
            scale: if motion {
                (1.0 - 0.2 * ease, 1.0 - 0.2 * ease)
            } else {
                (1.0, 1.0)
            },
            alpha: (1.0 - ease) as f32,
            pivot: (0.5, 0.5),
        },
        (Kind::Dialog, Effect::Map) => Frame {
            scale: (1.0, if motion { ease } else { 1.0 }),
            alpha: if motion { ease as f32 } else { 1.0 },
            pivot: (0.5, 0.5),
        },
        (Kind::Dialog, Effect::Destroy) => Frame {
            scale: (1.0, if motion { 1.0 - ease } else { 1.0 }),
            alpha: if motion { 1.0 } else { (1.0 - ease) as f32 },
            pivot: (0.5, 0.5),
        },
    }
}
pub fn rect(geometry: Rect, frame: Frame) -> RectF {
    let w = f64::from(geometry.size.w);
    let h = f64::from(geometry.size.h);
    Rectangle::new(
        (
            f64::from(geometry.loc.x) + w * (1.0 - frame.scale.0) * frame.pivot.0,
            f64::from(geometry.loc.y) + h * (1.0 - frame.scale.1) * frame.pivot.1,
        )
            .into(),
        (w * frame.scale.0, h * frame.scale.1).into(),
    )
}
/// Lifecycle snapshots do not carry workspace transforms. Suppress them during
/// both keyboard slides and gestures rather than leaking content across spaces.
pub fn effects_allowed(overview: bool, gesture: bool, slide: bool, locked: bool) -> bool {
    !overview && !gesture && !slide && !locked
}
fn interrupted_close(mapping: bool, resizing: bool) -> Option<&'static str> {
    if mapping {
        Some("interrupted-map")
    } else if resizing {
        Some("interrupted-size-change")
    } else {
        None
    }
}
fn snapshot_allowed(count: usize, retained: u64, pixels: u64) -> bool {
    count < 32 && retained.saturating_add(pixels) <= 16 * 1024 * 1024
}
fn successors(stack: &[u64], visible: &HashSet<u64>) -> Vec<Option<u64>> {
    let mut result = vec![None; stack.len()];
    let mut next = None;
    for (rank, id) in stack.iter().enumerate().rev() {
        result[rank] = next;
        if visible.contains(id) {
            next = Some(*id);
        }
    }
    result
}
fn retire<T>(items: &mut Vec<T>, expired: impl Fn(&T) -> bool) -> Vec<T> {
    let mut removed = Vec::new();
    let mut active = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if expired(&item) {
            removed.push(item);
        } else {
            active.push(item);
        }
    }
    *items = active;
    removed
}
struct Cached {
    geometry: Rect,
    scale: f64,
    kind: Kind,
    parent: Option<u64>,
    resizing: bool,
    order: usize,
    elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
}
struct Running {
    kind: Kind,
    start: Duration,
}
pub struct Ghost {
    pub id: u64,
    pub before: Option<u64>,
    order: usize,
    parent: Option<u64>,
    geometry: Rect,
    snapshot: Snapshot,
    pixels: u64,
    running: Running,
}
#[derive(Default)]
pub struct Lifecycle {
    known: HashSet<u64>,
    cache: HashMap<u64, Cached>,
    stack: Vec<u64>,
    opening: HashMap<u64, Running>,
    ghosts: Vec<Ghost>,
    history: VecDeque<serde_json::Value>,
    now: Duration,
    policy: MotionPolicy,
}
impl Lifecycle {
    fn suspend(&mut self, ids: HashSet<u64>) {
        self.opening.clear();
        self.ghosts.clear();
        self.cache.clear();
        self.stack.clear();
        self.known = ids;
    }
    pub fn running(&self) -> bool {
        !self.opening.is_empty() || !self.ghosts.is_empty()
    }

    pub fn opening_frame(&self, id: u64) -> Option<Frame> {
        let run = self.opening.get(&id)?;
        let d = duration(run.kind, self.policy).as_secs_f64();
        Some(frame(
            run.kind,
            Effect::Map,
            if d > 0.0 {
                self.now.saturating_sub(run.start).as_secs_f64() / d
            } else {
                1.0
            },
            self.policy,
        ))
    }
    pub fn ghosts(&self) -> impl Iterator<Item = &Ghost> {
        self.ghosts.iter()
    }
    fn destroy_frame(&self, ghost: &Ghost) -> Frame {
        let d = duration(ghost.running.kind, self.policy).as_secs_f64();
        frame(
            ghost.running.kind,
            Effect::Destroy,
            if d > 0.0 {
                self.now.saturating_sub(ghost.running.start).as_secs_f64() / d
            } else {
                1.0
            },
            self.policy,
        )
    }
    pub fn element(
        &self,
        ghost: &Ghost,
        renderer: &GlesRenderer,
        view: crate::runtime::View,
    ) -> Option<
        smithay::backend::renderer::element::texture::TextureRenderElement<
            smithay::backend::renderer::gles::GlesTexture,
        >,
    > {
        let f = self.destroy_frame(ghost);
        let geometry = rect(ghost.geometry, f);
        crate::size_change::snapshot_element(
            renderer,
            &ghost.snapshot,
            &crate::size_change::Frame {
                live: geometry,
                live_scale: f.scale,
                live_visible: false,
                snapshot: geometry,
                snapshot_alpha: f.alpha,
            },
            view,
        )
    }
    fn record(&mut self, id: u64, effect: &str, kind: Kind, outcome: &str) {
        self.history.push_back(serde_json::json!({"window":id,"effect":effect,"kind":format!("{kind:?}"),"outcome":outcome,"at_ms":self.now.as_millis(),"duration_ms":duration(kind,self.policy).as_millis()}));
        while self.history.len() > 32 {
            self.history.pop_front();
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        let frame_json = |f: Frame| {
            serde_json::json!({
                "scale": f.scale, "alpha": f.alpha, "pivot": f.pivot,
            })
        };
        let opening: Vec<_> = self
            .opening
            .iter()
            .map(|(id, run)| {
                serde_json::json!({
                    "window": id, "kind": format!("{:?}",run.kind),
                    "elapsed_ms": self.now.saturating_sub(run.start).as_millis(),
                    "duration_ms": duration(run.kind,self.policy).as_millis(),
                    "frame": self.opening_frame(*id).map(frame_json),
                })
            })
            .collect();
        let destroying: Vec<_> = self
            .ghosts
            .iter()
            .map(|g| {
                serde_json::json!({
                    "window": g.id, "kind": format!("{:?}",g.running.kind),
                    "elapsed_ms": self.now.saturating_sub(g.running.start).as_millis(),
                    "duration_ms": duration(g.running.kind,self.policy).as_millis(),
                    "frame": frame_json(self.destroy_frame(g)),
                })
            })
            .collect();
        serde_json::json!({
            "retained_destroy_bytes": self.ghosts.iter().map(|g|g.pixels*4).sum::<u64>(),
            "snapshot_budget_bytes": 64*1024*1024,
            "snapshot_effect_limit": 32,
            "opening": opening, "destroying": destroying, "history": self.history,
        })
    }
}

pub fn step_frame(
    manager: &mut crate::windows::WindowManager,
    renderer: &mut GlesRenderer,
    state: &crate::State,
    now: Duration,
    policy: MotionPolicy,
    allowed: bool,
) {
    use smithay::backend::renderer::element::{
        surface::render_elements_from_surface_tree, Kind as RenderKind,
    };
    use smithay::wayland::seat::WaylandFocus;
    let ids: HashSet<u64> = manager.model().windows().map(|w| w.id).collect();
    let buffered: HashSet<u64> = ids
        .iter()
        .copied()
        .filter(|id| {
            manager
                .surface_of(*id)
                .as_ref()
                .and_then(crate::windows::committed_size)
                .is_some_and(|size| size.w > 0 && size.h > 0)
        })
        .collect();
    let entries = manager.render_entries();
    let mut shown = Vec::new();
    if allowed && policy.allows_fades() {
        for (index, (id, window, geometry)) in entries.iter().enumerate() {
            let Some(kind) = manager.lifecycle_kind(*id) else {
                continue;
            };
            let Some(surface) = window.wl_surface() else {
                continue;
            };
            let Some(size) = crate::windows::committed_size(surface.as_ref()) else {
                continue;
            };
            if size.w <= 0 || size.h <= 0 {
                continue;
            }
            let scale = state.scale_for(*geometry).fractional_scale();
            let geo = crate::popup::window_geometry_loc(&surface);
            let origin: smithay::utils::Point<i32, smithay::utils::Physical> = (
                (-f64::from(geo.x) * scale).round() as i32,
                (-f64::from(geo.y) * scale).round() as i32,
            )
                .into();
            let elements = render_elements_from_surface_tree(
                renderer,
                &surface,
                origin,
                scale,
                1.0,
                RenderKind::Unspecified,
            );
            if elements.is_empty() {
                continue;
            }
            shown.push((
                *id,
                Cached {
                    geometry: Rect::new(geometry.loc, size),
                    scale,
                    kind,
                    parent: manager.transient_parent(*id),
                    resizing: manager.size_changes().frame(*id).is_some(),
                    order: index,
                    elements,
                },
            ));
        }
    }
    let life = manager.window_lifecycle_mut();
    life.now = now;
    life.policy = policy;
    if !allowed || !policy.allows_fades() {
        life.suspend(ids);
        return;
    }
    // Release expired textures before admitting new snapshots to the budget.
    let finished = retire(&mut life.ghosts, |g| {
        now.saturating_sub(g.running.start) >= duration(g.running.kind, policy)
            || g.parent.is_some_and(|p| !ids.contains(&p))
    });
    for g in finished {
        life.record(g.id, "destroy", g.running.kind, "completed");
    }
    let visible_ids = entries.iter().map(|e| e.0).collect();
    let next = successors(&life.stack, &visible_ids);
    let gone: Vec<_> = life
        .cache
        .keys()
        .copied()
        .filter(|id| !ids.contains(id))
        .collect();
    for id in gone {
        let old = life.cache.remove(&id).expect("cached id");
        let mapping = life.opening.remove(&id).is_some();
        // Raw cached elements exclude active map/resize transforms. A safe
        // immediate close is preferable to an opaque full-size frozen jump.
        if let Some(reason) = interrupted_close(mapping, old.resizing) {
            life.record(id, "destroy", old.kind, reason);
            continue;
        }
        if old.parent.is_some_and(|parent| !ids.contains(&parent)) {
            continue;
        }
        let pixels = ((f64::from(old.geometry.size.w) * old.scale).round() as u64)
            .saturating_mul((f64::from(old.geometry.size.h) * old.scale).round() as u64);
        // Bound retained GPU copies to 64 MiB and 32 simultaneous effects.
        if !snapshot_allowed(
            life.ghosts.len(),
            life.ghosts.iter().map(|g| g.pixels).sum(),
            pixels,
        ) {
            life.record(id, "destroy", old.kind, "snapshot-budget");
            continue;
        }
        if let Some(texture) = copy_frame(renderer, &old) {
            life.record(id, "destroy", old.kind, "started");
            life.ghosts.push(Ghost {
                id,
                before: next.get(old.order).copied().flatten(),
                order: old.order,
                parent: old.parent,
                geometry: old.geometry,
                pixels,
                snapshot: Snapshot {
                    texture,
                    id: smithay::backend::renderer::element::Id::new(),
                },
                running: Running {
                    kind: old.kind,
                    start: now,
                },
            });
        } else {
            life.record(id, "destroy", old.kind, "snapshot-failed");
        }
    }
    life.ghosts.sort_by_key(|g| g.order);
    let completed: Vec<_> = life
        .opening
        .iter()
        .filter(|(id, r)| {
            !ids.contains(id) || now.saturating_sub(r.start) >= duration(r.kind, policy)
        })
        .map(|(id, r)| (*id, r.kind))
        .collect();
    for (id, kind) in completed {
        life.opening.remove(&id);
        life.record(id, "map", kind, "completed");
    }
    let visible: HashSet<_> = shown.iter().map(|e| e.0).collect();
    life.cache.retain(|id, _| visible.contains(id));
    for (id, cached) in shown {
        if life.known.insert(id) {
            life.opening.insert(
                id,
                Running {
                    kind: cached.kind,
                    start: now,
                },
            );
            life.record(id, "map", cached.kind, "started");
        }
        life.cache.insert(id, cached);
    }
    life.stack = entries.iter().map(|e| e.0).collect();
    // A first commit on a hidden workspace is a map too: showing that
    // workspace later must not replay a fresh-window effect.
    life.known.extend(buffered);
    life.known.retain(|id| ids.contains(id));
}
fn copy_frame(
    renderer: &mut GlesRenderer,
    cached: &Cached,
) -> Option<smithay::backend::renderer::gles::GlesTexture> {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::{
        utils::draw_render_elements, Bind, Color32F, Frame as _, Offscreen, Renderer,
    };
    use smithay::utils::{Physical, Size, Transform};
    let size: Size<i32, Physical> = (
        (f64::from(cached.geometry.size.w) * cached.scale).round() as i32,
        (f64::from(cached.geometry.size.h) * cached.scale).round() as i32,
    )
        .into();
    let mut texture: smithay::backend::renderer::gles::GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())
        .ok()?;
    {
        let mut target = renderer.bind(&mut texture).ok()?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
        let all = Rectangle::from_size(size);
        frame
            .clear(Color32F::new(0.0, 0.0, 0.0, 0.0), &[all])
            .ok()?;
        draw_render_elements(&mut frame, cached.scale, &cached.elements, &[all]).ok()?;
        let _ = frame.finish().ok()?;
    }
    Some(texture)
}
#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::motion::MotionLevel;
    fn policy(level: MotionLevel) -> MotionPolicy {
        MotionPolicy::new(level, 1.0)
    }
    #[test]
    fn keyboard_workspace_slide_suppresses_lifecycle_and_resume_does_not_replay_map() {
        assert!(effects_allowed(false, false, false, false));
        assert!(!effects_allowed(false, false, true, false));
        let mut life = Lifecycle::default();
        life.opening.insert(
            7,
            Running {
                kind: Kind::Normal,
                start: Duration::ZERO,
            },
        );
        life.stack.push(7);
        life.suspend(HashSet::from([7]));
        assert!(!life.running());
        assert!(life.stack.is_empty());
        assert!(!life.known.insert(7));
    }
    #[test]
    fn interrupted_map_and_size_change_close_skip_raw_snapshot() {
        assert_eq!(interrupted_close(true, false), Some("interrupted-map"));
        assert_eq!(
            interrupted_close(false, true),
            Some("interrupted-size-change")
        );
        assert_eq!(interrupted_close(false, false), None);
    }
    #[test]
    fn expired_ghosts_release_budget_before_new_close() {
        let mut retained = vec![Duration::ZERO; 32];
        let removed = retire(&mut retained, |start| {
            Duration::from_millis(150).saturating_sub(*start) >= Duration::from_millis(150)
        });
        assert_eq!(removed.len(), 32);
        assert!(snapshot_allowed(retained.len(), 0, 1024));
        assert!(!snapshot_allowed(32, 0, 1));
        assert!(!snapshot_allowed(0, u64::MAX, 1));
    }
    #[test]
    fn shared_stack_successors_preserve_order_without_hidden_windows() {
        assert_eq!(
            successors(&[1, 2, 3, 4], &HashSet::from([1, 4])),
            vec![Some(4), Some(4), Some(4), None]
        );
    }
    #[test]
    fn gnome51_normal_map_pivot_and_exponential_curve() {
        let p = policy(MotionLevel::Full);
        let start = frame(Kind::Normal, Effect::Map, 0.0, p);
        assert_eq!(start.scale, (0.01, 0.05));
        assert_eq!(start.alpha, 0.0);
        assert_eq!(start.pivot, (0.5, 1.0));
        let mid = frame(Kind::Normal, Effect::Map, 0.5, p);
        assert_eq!(mid.alpha, 0.96875);
        assert_eq!(frame(Kind::Normal, Effect::Map, 1.0, p).scale, (1.0, 1.0));
        assert_eq!(duration(Kind::Normal, p), Duration::from_millis(150));
    }
    #[test]
    fn destroy_and_attached_dialog_follow_quad() {
        let p = policy(MotionLevel::Full);
        let mid = frame(Kind::Normal, Effect::Destroy, 0.5, p);
        assert_eq!(mid.scale, (0.85, 0.85));
        assert_eq!(mid.alpha, 0.25);
        let d = frame(Kind::Dialog, Effect::Destroy, 0.5, p);
        assert_eq!(d.scale, (1.0, 0.25));
        assert_eq!(d.alpha, 1.0);
        assert_eq!(duration(Kind::Dialog, p), Duration::from_millis(100));
    }
    #[test]
    fn reduced_motion_and_slowdown() {
        let p = policy(MotionLevel::FadeOnly);
        let f = frame(Kind::Normal, Effect::Destroy, 0.5, p);
        assert_eq!(f.scale, (1.0, 1.0));
        assert_eq!(f.alpha, 0.25);
        let d = frame(Kind::Dialog, Effect::Map, 0.0, p);
        assert_eq!(d.scale, (1.0, 1.0));
        assert_eq!(d.alpha, 1.0);
        assert_eq!(
            duration(Kind::Normal, MotionPolicy::new(MotionLevel::Full, 2.0)),
            Duration::from_millis(300)
        );
    }
    #[test]
    fn animations_off_land_without_scale_or_fade() {
        let p = policy(MotionLevel::Off);
        let opened = frame(Kind::Normal, Effect::Map, 0.0, p);
        assert_eq!(opened.scale, (1.0, 1.0));
        assert_eq!(opened.alpha, 1.0);
        assert_eq!(frame(Kind::Dialog, Effect::Destroy, 0.0, p).alpha, 0.0);
        assert_eq!(duration(Kind::Normal, p), Duration::ZERO);
    }
    #[test]
    fn pivot_keeps_bottom_of_map_and_centre_of_destroy_fixed() {
        let r = Rect::new((100, 200).into(), (400, 300).into());
        let p = policy(MotionLevel::Full);
        let mapped = rect(r, frame(Kind::Normal, Effect::Map, 0.0, p));
        assert_eq!(mapped.loc.y + mapped.size.h, 500.0);
        assert_eq!(mapped.loc.x + mapped.size.w / 2.0, 300.0);
        let closed = rect(r, frame(Kind::Normal, Effect::Destroy, 1.0, p));
        assert_eq!(closed.loc.y + closed.size.h / 2.0, 350.0);
    }
}
