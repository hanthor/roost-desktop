//! Overview window previews (#54): the compositor half of GNOME's
//! Activities overview.
//!
//! In GNOME 51 the overview shows the active workspace as a large card
//! (the screen scaled down) holding live window previews spread so none
//! overlap, with the neighboring workspaces peeking in from the sides.
//! The shell draws the search entry above and the dash below; the
//! compositor draws the cards and previews because it owns the window
//! surfaces (no capture protocol, nothing privileged handed out).
//!
//! Everything here is pure geometry so it is unit-tested without a
//! renderer; the runtime draws [`OverviewLayout`] and hit-tests it.
//! Geometry follows GNOME Shell 51 (workspacesView.js), checked against
//! GNOME itself at 1280x800 (docs/gnome-parity.md): the card is the work
//! area below the top bar scaled by 0.72 (922x553 at 179,108); neighbors
//! sit one window picker (the card plus 20px each side) and 24px
//! (`WORKSPACE_MIN_SPACING`) away, scaled by 0.94
//! (`WORKSPACE_INACTIVE_SCALE`); and, as GNOME's dynamic workspaces
//! always keep one, an empty workspace follows the last occupied one.

use smithay::utils::{Logical, Point, Rectangle, Size};
use tuna_shell_control::MotionPolicy;

/// Card size as a fraction of the work area (GNOME 51: 922x553 on a
/// 1280x768 work area).
pub const CARD_SCALE: f64 = 0.72;
/// Card top edge as a fraction of the output height (search sits above).
pub const CARD_TOP: f64 = 0.135;
/// The window picker's padding around the card, each side.
pub const PICKER_PAD_X: i32 = 20;
/// The window picker's padding above and below the card.
pub const PICKER_PAD_Y: i32 = 12;
/// Space between neighboring window pickers (`WORKSPACE_MIN_SPACING`).
pub const WORKSPACE_SPACING: i32 = 24;
/// Neighboring workspaces are drawn smaller (`WORKSPACE_INACTIVE_SCALE`).
pub const INACTIVE_SCALE: f64 = 0.94;

/// One mapped window as the overview sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewWindow {
    /// Compositor window id.
    pub id: u64,
    /// Workspace it lives on.
    pub workspace: u32,
    /// Its desktop geometry (global logical space).
    pub geometry: Rectangle<i32, Logical>,
}

/// A workspace drawn as a card.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkspaceCard {
    /// Workspace id (for GNOME's trailing empty workspace, one past the
    /// last; switching to it creates it).
    pub workspace: u32,
    /// Where the card sits on the output.
    pub rect: Rectangle<i32, Logical>,
    /// Whether this is the active (center) card.
    pub active: bool,
    /// Opacity while the card joins the overview transition.
    pub alpha: f32,
    /// The card's size once the transition settles. Transition frames
    /// resize `rect`; the drawn wallpaper card is rendered once at this
    /// size and scaled, rather than re-rendered for every frame's size.
    pub settled: Size<i32, Logical>,
}

/// One window preview: where to draw the window's surface and at what
/// scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preview {
    /// Window id.
    pub id: u64,
    /// The preview's on-screen rectangle (scaled window).
    pub rect: Rectangle<i32, Logical>,
    /// Surface scale (preview size over window size).
    pub scale: f64,
    /// Whether it sits on the active card (clickable to focus).
    pub active: bool,
    /// Opacity (a dragged preview fades).
    pub alpha: f32,
}

/// The whole overview scene for one output.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverviewLayout {
    /// Cards, active first.
    pub cards: Vec<WorkspaceCard>,
    /// Previews bottom to top (active card last, so it paints on top).
    pub previews: Vec<Preview>,
    /// The active preview under the pointer, grown (see [`grow_hovered`]).
    pub hovered: Option<u64>,
    /// GNOME's workspace thumbnails strip, left to right (empty below
    /// [`THUMBNAILS_MIN_WORKSPACES`]).
    pub thumbnails: Vec<WorkspaceCard>,
    /// The new-workspace drop slot, visible only during a window drag.
    pub placeholder: Option<(u32, Rectangle<i32, Logical>)>,
}

/// GNOME shows the thumbnails strip once dynamic workspaces number more
/// than two (`NUM_WORKSPACES_THRESHOLD`, workspaceThumbnail.js).
pub const THUMBNAILS_MIN_WORKSPACES: usize = 3;
/// Thumbnail size as a fraction of the work area (`MAX_THUMBNAIL_SCALE`).
pub const THUMBNAIL_STRIP_SCALE: f64 = 0.034;
/// The strip's top edge as a fraction of the output height (below the
/// search entry: y 96 on 1280x800).
pub const THUMBNAILS_TOP: f64 = 0.12;
/// Padding inside the strip and spacing between thumbnails
/// (`.workspace-thumbnails { padding: 6px; spacing: 6px }`).
pub const THUMBNAILS_PAD: i32 = 6;
/// The active thumbnail's indicator border (`.workspace-thumbnail-indicator
/// { border: 3px; border-radius: 8px }`).
pub const THUMBNAIL_INDICATOR_BORDER: i32 = 3;
/// Corner radius of a thumbnail (`.workspace-thumbnail { border-radius: 4px }`).
pub const THUMBNAIL_RADIUS: i32 = 4;
/// Corner radius of the active indicator.
pub const THUMBNAIL_INDICATOR_RADIUS: i32 = 8;

/// How far the strip pushes the workspace card down (and shrinks it):
/// the strip's height plus the controls spacing.
fn thumbnails_offset(work_h: i32) -> i32 {
    thumbnail_size_for(1, work_h).1 + 2 * THUMBNAILS_PAD + THUMBNAILS_PAD
}

/// A thumbnail's size for an output `w` wide over a `work_h` work area
/// (`w` only matters for the width).
fn thumbnail_size_for(w: i32, work_h: i32) -> (i32, i32) {
    (
        (f64::from(w) * THUMBNAIL_STRIP_SCALE).floor() as i32,
        (f64::from(work_h) * THUMBNAIL_STRIP_SCALE).floor() as i32,
    )
}

/// GNOME's hover growth (`WINDOW_ACTIVE_SIZE_INC`): 5px each side.
pub const HOVER_GROWTH: i32 = 5;

/// The topmost active preview under `pointer`: the one GNOME grows.
pub fn hovered_at(layout: &OverviewLayout, pointer: Point<f64, Logical>) -> Option<u64> {
    layout
        .previews
        .iter()
        .rev()
        .find(|p| p.active && p.rect.to_f64().contains(pointer))
        .map(|p| p.id)
}

/// Grow the topmost active preview under `pointer` by
/// [`HOVER_GROWTH`] a side, as GNOME does, and remember it as hovered.
pub fn grow_hovered(layout: &mut OverviewLayout, pointer: Point<f64, Logical>) {
    let Some(id) = hovered_at(layout, pointer) else {
        return;
    };
    grow_preview(layout, id, 1.0);
    layout.hovered = Some(id);
}

/// Grow window `id`'s active preview by `amount` (0..1) of
/// [`HOVER_GROWTH`] a side: GNOME's hover scale part-way through its
/// 200 ms tween. The surface scale follows continuously; the rect
/// snaps to whole logical pixels.
pub fn grow_preview(layout: &mut OverviewLayout, id: u64, amount: f64) {
    let amount = amount.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return;
    }
    let Some(p) = layout
        .previews
        .iter_mut()
        .rev()
        .find(|p| p.active && p.id == id)
    else {
        return;
    };
    let growth = f64::from(HOVER_GROWTH) * amount;
    let w = f64::from(p.rect.size.w.max(1));
    p.scale *= (w + 2.0 * growth) / w;
    let g = growth.round() as i32;
    p.rect = Rectangle::new(
        (p.rect.loc.x - g, p.rect.loc.y - g).into(),
        (p.rect.size.w + 2 * g, p.rect.size.h + 2 * g).into(),
    );
}

/// GNOME's window drag in the overview (windowPreview.js): the
/// dragged preview shrinks to fit [`WINDOW_DND_SIZE`] and fades to
/// [`DRAGGING_WINDOW_OPACITY`].
pub const WINDOW_DND_SIZE: f64 = 256.0;
/// Opacity of a dragged preview (100 of 255).
pub const DRAGGING_WINDOW_OPACITY: f32 = 100.0 / 255.0;
/// Pointer travel before a press on a preview becomes a drag
/// (`drag-threshold`).
pub const DRAG_THRESHOLD: f64 = 8.0;

/// Turn window `id`'s active preview into the dragged one: shrunk to
/// fit [`WINDOW_DND_SIZE`] about the point grabbed at `start`, that
/// point following the pointer to `pos`, faded, and drawn on top.
pub fn drag_preview(
    layout: &mut OverviewLayout,
    id: u64,
    start: Point<f64, Logical>,
    pos: Point<f64, Logical>,
) {
    drag_preview_shrinking(layout, id, start, pos, 1.0);
}

/// [`drag_preview`] part-way through GNOME's 250 ms drag shrink
/// (`SCALE_ANIMATION_TIME`): `shrink` 0 keeps the preview's size, 1 is
/// the full [`WINDOW_DND_SIZE`] fit. The opacity drops at once, as
/// GNOME's `dragActorOpacity` does.
pub fn drag_preview_shrinking(
    layout: &mut OverviewLayout,
    id: u64,
    start: Point<f64, Logical>,
    pos: Point<f64, Logical>,
    shrink: f64,
) {
    let Some(index) = layout.previews.iter().position(|p| p.active && p.id == id) else {
        return;
    };
    let mut p = layout.previews.remove(index);
    let (w, h) = (f64::from(p.rect.size.w), f64::from(p.rect.size.h));
    let fit = (WINDOW_DND_SIZE / w.max(h).max(1.0)).min(1.0);
    let k = 1.0 + (fit - 1.0) * shrink.clamp(0.0, 1.0);
    let (fx, fy) = (
        (start.x - f64::from(p.rect.loc.x)) / w.max(1.0),
        (start.y - f64::from(p.rect.loc.y)) / h.max(1.0),
    );
    let (nw, nh) = ((w * k).round(), (h * k).round());
    p.rect = Rectangle::new(
        (
            (pos.x - fx * nw).round() as i32,
            (pos.y - fy * nh).round() as i32,
        )
            .into(),
        (nw as i32, nh as i32).into(),
    );
    p.scale *= k;
    p.alpha = DRAGGING_WINDOW_OPACITY;
    // Not a click target while it rides the pointer.
    p.active = false;
    layout.previews.push(p);
}

/// Insertion gaps before/between/after thumbnails. GNOME opens a slot
/// when the pointer enters a gap rather than an existing thumbnail.
pub fn insertion_target(layout: &OverviewLayout, pos: Point<f64, Logical>) -> Option<u32> {
    let first = layout.thumbnails.first()?;
    let last = layout.thumbnails.last()?;
    if pos.y < f64::from(first.rect.loc.y)
        || pos.y >= f64::from(first.rect.loc.y + first.rect.size.h)
    {
        return None;
    }
    let pad = f64::from(THUMBNAILS_PAD);
    const CUT: f64 = 10.0;
    if pos.x >= f64::from(first.rect.loc.x) - pad && pos.x < f64::from(first.rect.loc.x) + CUT {
        return Some(first.workspace);
    }
    for pair in layout.thumbnails.windows(2) {
        let left = pair[0].rect;
        if pos.x >= f64::from(left.loc.x + left.size.w) && pos.x < f64::from(pair[1].rect.loc.x) {
            return Some(pair[1].workspace);
        }
    }
    // The last shown thumbnail is GNOME's trailing empty workspace;
    // use its slot for an end drop rather than leaving an extra empty gap.
    let end = f64::from(last.rect.loc.x + last.rect.size.w);
    (pos.x >= end && pos.x <= end + pad + f64::from(last.rect.size.w)).then_some(last.workspace)
}

/// Spread the thumbnails around a new-workspace slot, keeping their
/// miniature windows aligned with their cards.
pub fn show_placeholder(layout: &mut OverviewLayout, at: u32) {
    let Some(first) = layout.thumbnails.first() else {
        return;
    };
    let size = first.rect.size;
    let step = 18 + THUMBNAILS_PAD;
    let x = layout
        .thumbnails
        .iter()
        .find(|t| t.workspace >= at)
        .map(|t| t.rect.loc.x)
        .unwrap_or_else(|| {
            let t = layout.thumbnails.last().unwrap();
            t.rect.loc.x + t.rect.size.w + THUMBNAILS_PAD
        });
    let y = first.rect.loc.y;
    // Match each miniature against the original strip once. Moving it
    // first can otherwise place it inside the next thumbnail's old bounds.
    let original_thumbnails: Vec<_> = layout
        .thumbnails
        .iter()
        .map(|thumb| {
            let offset = if thumb.workspace >= at { step } else { 0 };
            (thumb.rect, offset)
        })
        .collect();
    for preview in &mut layout.previews {
        if !preview.active {
            if let Some((_, offset)) = original_thumbnails
                .iter()
                .find(|(rect, _)| rect.contains(preview.rect.loc))
            {
                preview.rect.loc.x += offset;
            }
        }
    }
    for (thumb, (_, offset)) in layout.thumbnails.iter_mut().zip(original_thumbnails) {
        thumb.rect.loc.x += offset;
    }
    layout.placeholder = Some((at, Rectangle::new((x, y).into(), (18, size.h).into())));
}

/// The workspace a window dropped at `pos` goes to: a thumbnail, or a
/// neighboring card.
pub fn drop_target(layout: &OverviewLayout, pos: Point<f64, Logical>) -> Option<u32> {
    layout
        .thumbnails
        .iter()
        .find(|t| t.rect.to_f64().contains(pos))
        .or_else(|| {
            layout
                .cards
                .iter()
                .find(|c| !c.active && c.rect.to_f64().contains(pos))
        })
        .map(|c| c.workspace)
}

/// GNOME's overview transition time (`ANIMATION_TIME`, 250 ms).
pub const TRANSITION_MS: f64 = 250.0;

/// GNOME's `EASE_OUT_QUAD` for the overview transition.
pub fn ease_out_quad(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

/// GNOME's `EASE_OUT_SINE`: how the overview opens.
pub fn ease_out_sine(t: f64) -> f64 {
    (t.clamp(0.0, 1.0) * std::f64::consts::FRAC_PI_2).sin()
}

/// GNOME's `EASE_OUT_CUBIC`: gesture completion and workspace scrolls.
pub fn ease_out_cubic(t: f64) -> f64 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// The Clutter animation modes the overview uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    EaseOutQuad,
    EaseOutSine,
    EaseOutCubic,
}

impl Curve {
    /// Eased fraction at linear time fraction `t`.
    pub fn apply(self, t: f64) -> f64 {
        match self {
            Curve::EaseOutQuad => ease_out_quad(t),
            Curve::EaseOutSine => ease_out_sine(t),
            Curve::EaseOutCubic => ease_out_cubic(t),
        }
    }

    /// The name proofs and state snapshots use.
    pub fn name(self) -> &'static str {
        match self {
            Curve::EaseOutQuad => "ease-out-quad",
            Curve::EaseOutSine => "ease-out-sine",
            Curve::EaseOutCubic => "ease-out-cubic",
        }
    }
}

/// Closing eases with GNOME's `EASE_OUT_QUAD` (`animateFromOverview`).
pub const CLOSE_CURVE: Curve = Curve::EaseOutQuad;
/// Opening eases with `EASE_OUT_SINE` (`animateToOverview`).
pub const OPEN_CURVE: Curve = Curve::EaseOutSine;
/// Window picker and app grid swap (`SIDE_CONTROLS_ANIMATION_TIME`).
pub const SIDE_CONTROLS_MS: f64 = 250.0;
/// The app grid's own fade (appDisplay.js), drawn by the shell.
pub const APP_GRID_FADE_MS: f64 = 400.0;
/// The app grid fade waits this long after the swap starts.
pub const APP_GRID_FADE_DELAY_MS: f64 = 100.0;
/// Hover grow and overlay fade (`WINDOW_SCALE_TIME`,
/// `WINDOW_OVERLAY_FADE_TIME`).
pub const HOVER_MS: f64 = 200.0;
/// Thumbnails slide in and collapse (`SLIDE_ANIMATION_TIME`,
/// `RESCALE_ANIMATION_TIME`).
pub const THUMBNAIL_MS: f64 = 200.0;
/// A new window's preview scales in (workspace.js `_doAddWindow`).
pub const NEW_PREVIEW_MS: f64 = 250.0;
/// Drag shrink (`SCALE_ANIMATION_TIME`).
pub const DRAG_SCALE_MS: f64 = 250.0;
/// A drag dropped on nothing glides home (`SNAP_BACK_ANIMATION_TIME`).
pub const SNAP_BACK_MS: f64 = 250.0;
/// A drop that was accepted but changed nothing fades the preview back
/// in where it was (`REVERT_ANIMATION_TIME`).
pub const REVERT_MS: f64 = 750.0;
/// Workspace scroll inside the overview (`WORKSPACE_SWITCH_TIME`).
pub const WORKSPACE_SCROLL_MS: f64 = 250.0;

/// One eased value moving between two endpoints over a fixed time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    pub from: f64,
    pub to: f64,
    pub duration_ms: f64,
    pub elapsed_ms: f64,
    pub curve: Curve,
}

impl Tween {
    /// Resting at `value`.
    pub fn settled(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            duration_ms: 0.0,
            elapsed_ms: 0.0,
            curve: Curve::EaseOutQuad,
        }
    }

    pub fn new(from: f64, to: f64, duration_ms: f64, curve: Curve) -> Self {
        Self {
            from,
            to,
            duration_ms,
            elapsed_ms: 0.0,
            curve,
        }
    }

    /// Linear time fraction, 0..1.
    pub fn time(&self) -> f64 {
        if self.duration_ms <= 0.0 {
            1.0
        } else {
            (self.elapsed_ms / self.duration_ms).clamp(0.0, 1.0)
        }
    }

    pub fn value(&self) -> f64 {
        self.from + (self.to - self.from) * self.curve.apply(self.time())
    }

    pub fn done(&self) -> bool {
        self.time() >= 1.0
    }

    pub fn step(&mut self, dt_ms: f64) {
        self.elapsed_ms += dt_ms.max(0.0);
    }

    /// Head for `to` from wherever the value is now (GNOME's `ease`
    /// replacing a running transition), or snap when `animate` is off.
    pub fn retarget(&mut self, to: f64, duration_ms: f64, curve: Curve, animate: bool) {
        if self.to == to {
            return;
        }
        *self = if animate {
            Self::new(self.value(), to, duration_ms, curve)
        } else {
            Self::settled(to)
        };
    }
}

/// Whether `preview` is a miniature in the thumbnails strip.
fn in_thumbnail(layout: &OverviewLayout, preview: &Preview) -> bool {
    let center = preview.rect.loc
        + Point::<i32, Logical>::from((preview.rect.size.w / 2, preview.rect.size.h / 2));
    layout.thumbnails.iter().any(|t| t.rect.contains(center))
}

/// `rect` scaled by `k` about its center.
fn scale_about_center(rect: Rectangle<i32, Logical>, k: f64) -> Rectangle<i32, Logical> {
    let (w, h) = (
        (f64::from(rect.size.w) * k).round() as i32,
        (f64::from(rect.size.h) * k).round() as i32,
    );
    Rectangle::new(
        (
            rect.loc.x + (rect.size.w - w) / 2,
            rect.loc.y + (rect.size.h - h) / 2,
        )
            .into(),
        (w, h).into(),
    )
}

/// A thumbnail `fraction` collapsed: narrowed about its center
/// (workspaceThumbnail.js `collapse-fraction`).
fn collapse(rect: Rectangle<i32, Logical>, fraction: f64) -> Rectangle<i32, Logical> {
    let w = (f64::from(rect.size.w) * (1.0 - fraction.clamp(0.0, 1.0))).round() as i32;
    Rectangle::new(
        (rect.loc.x + (rect.size.w - w) / 2, rect.loc.y).into(),
        (w, rect.size.h).into(),
    )
}

/// The scene `p` of the way from `from` to `to` (both settled overview
/// scenes): cards and thumbnails matched by workspace, previews by
/// window and by whether they sit in the strip. What only `to` has
/// arrives (a new window's preview scales in from its center, GNOME's
/// `_doAddWindow`; a new thumbnail widens out of nothing; anything else
/// fades in); what only `from` had leaves the same way in reverse.
pub fn blend(from: &OverviewLayout, to: &OverviewLayout, p: f64) -> OverviewLayout {
    if p >= 1.0 {
        return to.clone();
    }
    let p = p.max(0.0);
    let a = p as f32;
    let mix = |x: f32, y: f32| x + (y - x) * a;
    let mut out = OverviewLayout {
        hovered: to.hovered,
        placeholder: to.placeholder,
        ..Default::default()
    };
    for c in &to.cards {
        out.cards.push(
            match from.cards.iter().find(|o| o.workspace == c.workspace) {
                Some(o) => WorkspaceCard {
                    rect: lerp_rect(o.rect, c.rect, p),
                    alpha: mix(o.alpha, c.alpha),
                    ..*c
                },
                None => WorkspaceCard {
                    alpha: c.alpha * a,
                    ..*c
                },
            },
        );
    }
    for o in &from.cards {
        if !to.cards.iter().any(|c| c.workspace == o.workspace) {
            out.cards.push(WorkspaceCard {
                active: false,
                alpha: o.alpha * (1.0 - a),
                ..*o
            });
        }
    }
    for t in &to.thumbnails {
        out.thumbnails.push(
            match from.thumbnails.iter().find(|o| o.workspace == t.workspace) {
                Some(o) => WorkspaceCard {
                    rect: lerp_rect(o.rect, t.rect, p),
                    alpha: mix(o.alpha, t.alpha),
                    ..*t
                },
                None => WorkspaceCard {
                    rect: collapse(t.rect, 1.0 - p),
                    alpha: t.alpha * a,
                    ..*t
                },
            },
        );
    }
    for o in &from.thumbnails {
        if !to.thumbnails.iter().any(|t| t.workspace == o.workspace) {
            out.thumbnails.push(WorkspaceCard {
                rect: collapse(o.rect, p),
                active: false,
                alpha: o.alpha * (1.0 - a),
                ..*o
            });
        }
    }
    // Leaving previews paint underneath the arriving scene.
    for o in &from.previews {
        let strip = in_thumbnail(from, o);
        if !to
            .previews
            .iter()
            .any(|t| t.id == o.id && in_thumbnail(to, t) == strip)
        {
            out.previews.push(Preview {
                active: false,
                alpha: o.alpha * (1.0 - a),
                ..*o
            });
        }
    }
    for t in &to.previews {
        let strip = in_thumbnail(to, t);
        let old = from
            .previews
            .iter()
            .find(|o| o.id == t.id && in_thumbnail(from, o) == strip);
        out.previews.push(match old {
            Some(o) => Preview {
                rect: lerp_rect(o.rect, t.rect, p),
                scale: o.scale + (t.scale - o.scale) * p,
                alpha: mix(o.alpha, t.alpha),
                ..*t
            },
            None if t.active => Preview {
                rect: scale_about_center(t.rect, p),
                scale: t.scale * p,
                alpha: if p > 0.0 { t.alpha } else { 0.0 },
                ..*t
            },
            None => Preview {
                alpha: t.alpha * a,
                ..*t
            },
        });
    }
    out
}

/// Why the settled overview scene changed shape, which decides how
/// GNOME animates the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MorphCause {
    /// Window picker and app grid swapped (250 ms `EASE_OUT_SINE`).
    AppGrid,
    /// The active workspace changed (250 ms `EASE_OUT_CUBIC`).
    Workspace,
    /// Thumbnails were added or removed (200 ms).
    Thumbnails,
    /// Previews came or went, a new window's scaling in (250 ms).
    Windows,
    /// A drag dropped on nothing glides home (250 ms).
    SnapBack,
}

impl MorphCause {
    pub fn timing(self) -> (f64, Curve) {
        match self {
            MorphCause::AppGrid => (SIDE_CONTROLS_MS, Curve::EaseOutSine),
            MorphCause::Workspace => (WORKSPACE_SCROLL_MS, Curve::EaseOutCubic),
            MorphCause::Thumbnails => (THUMBNAIL_MS, Curve::EaseOutQuad),
            MorphCause::Windows => (NEW_PREVIEW_MS, Curve::EaseOutQuad),
            MorphCause::SnapBack => (SNAP_BACK_MS, Curve::EaseOutQuad),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            MorphCause::AppGrid => "app-grid",
            MorphCause::Workspace => "workspace",
            MorphCause::Thumbnails => "thumbnails",
            MorphCause::Windows => "windows",
            MorphCause::SnapBack => "snap-back",
        }
    }
}

/// A swipe driving the transition: where it began, the finger travel
/// toward open so far, and its recent `(time, delta)` events.
#[derive(Debug, Clone)]
struct Swipe {
    initial: f64,
    travel: f64,
    history: crate::workspace_slide::SwipeHistory,
}

/// What a settled scene's shape is, to notice when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SceneShape {
    app_grid: bool,
    active: Option<u32>,
    thumbnails: Vec<u32>,
    previews: Vec<u64>,
}

impl SceneShape {
    fn of(layout: &OverviewLayout, app_grid: bool) -> Self {
        let mut previews: Vec<u64> = layout
            .previews
            .iter()
            .filter(|p| p.active)
            .map(|p| p.id)
            .collect();
        previews.sort_unstable();
        Self {
            app_grid,
            active: layout.cards.iter().find(|c| c.active).map(|c| c.workspace),
            thumbnails: layout.thumbnails.iter().map(|t| t.workspace).collect(),
            previews,
        }
    }

    fn cause(&self, next: &Self) -> Option<MorphCause> {
        if self.app_grid != next.app_grid {
            Some(MorphCause::AppGrid)
        } else if self.active != next.active {
            Some(MorphCause::Workspace)
        } else if self.thumbnails != next.thumbnails {
            Some(MorphCause::Thumbnails)
        } else if self.previews != next.previews {
            Some(MorphCause::Windows)
        } else {
            None
        }
    }
}

/// Where a released overview swipe settles and how long it takes
/// (`SwipeTracker._getEndProgress` and `_endGesture` over the overview's
/// hidden/open snap points): `progress` now, `initial` where it began,
/// `velocity` in touchpad units per ms toward open over `distance`.
/// The same tracker as the workspace swipe ([`crate::workspace_slide`]),
/// over the overview's two snap points. GNOME breaks an exact half-way
/// tie toward closed; Tuna Desktop has always opened there, and keeps
/// doing so.
pub fn swipe_release(progress: f64, initial: f64, velocity: f64, distance: f64) -> (f64, f64) {
    crate::workspace_slide::swipe_release_over(
        progress,
        initial.round().clamp(0.0, 1.0),
        2,
        velocity,
        false,
        distance,
    )
}

/// Samples of one timed open or close, for proofs to check the curve.
#[derive(Debug, Clone, PartialEq)]
struct Trace {
    tween: Tween,
    samples: Vec<(f64, f64)>,
}

const TRACE_SAMPLES: usize = 64;

/// A released swipe: where it went, how fast and for how long.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Release {
    from: f64,
    target: f64,
    velocity: f64,
    duration_ms: f64,
}

/// What the overview should be doing this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverviewCues {
    pub open: bool,
    pub app_grid: bool,
    pub search: bool,
    /// GNOME's motion policy: fade-only keeps the fades and snaps
    /// everything that moves or scales; durations take the slow-down.
    pub motion: MotionPolicy,
}

/// Every overview animation in one place (GNOME 51's timings): the
/// open/close transition and the swipe that drives it, plus the
/// settled scene's own motion (picker and app grid, workspace scroll,
/// thumbnails, new previews, hover, drag and search). The runtime feeds
/// it frame times and cues; [`OverviewMotion::scene`] draws from it.
/// Nothing here runs while the overview is fully closed.
#[derive(Debug, Clone)]
pub struct OverviewMotion {
    progress: Tween,
    /// A swipe holds the transition.
    swipe: Option<Swipe>,
    open_trace: Option<Trace>,
    close_trace: Option<Trace>,
    release: Option<Release>,
    shape: Option<SceneShape>,
    morph: Option<(OverviewLayout, Tween, MorphCause)>,
    last_morph: Option<MorphCause>,
    pending: Option<MorphCause>,
    last_scene: Option<OverviewLayout>,
    search: Tween,
    hovered: Option<u64>,
    hover: Vec<(u64, Tween)>,
    drag_shrink: Tween,
    dragging: bool,
    revert: Option<(u64, Tween)>,
}

impl Default for OverviewMotion {
    fn default() -> Self {
        Self {
            progress: Tween::settled(0.0),
            swipe: None,
            open_trace: None,
            close_trace: None,
            release: None,
            shape: None,
            morph: None,
            last_morph: None,
            pending: None,
            last_scene: None,
            search: Tween::settled(0.0),
            hovered: None,
            hover: Vec::new(),
            drag_shrink: Tween::settled(0.0),
            dragging: false,
            revert: None,
        }
    }
}

impl OverviewMotion {
    /// The drawn transition: 0 the desktop, 1 the overview.
    pub fn progress(&self) -> f64 {
        self.progress.value()
    }

    /// Whether search has fully faded the workspaces out.
    pub fn search_covers(&self) -> bool {
        self.search.value() >= 1.0
    }

    /// Jump everything to its end state (motion turned off).
    pub fn snap(&mut self, open: bool) {
        self.progress = Tween::settled(if open { 1.0 } else { 0.0 });
        self.search = Tween::settled(self.search.to);
        self.morph = None;
        self.revert = None;
        self.drag_shrink = Tween::settled(self.drag_shrink.to);
        for (_, t) in &mut self.hover {
            *t = Tween::settled(t.to);
        }
    }

    /// Advance the open/close transition by `dt_ms` toward `open`:
    /// opening eases out-sine, closing out-quad, 250 ms either way from
    /// wherever it is (GNOME's `animateToOverview`/`animateFromOverview`).
    pub fn step_progress(&mut self, dt_ms: f64, open: bool, motion: MotionPolicy) {
        let animate = motion.allows_motion();
        if self.swipe.is_some() {
            return;
        }
        let target = if open { 1.0 } else { 0.0 };
        if self.progress.to != target {
            let curve = if open { OPEN_CURVE } else { CLOSE_CURVE };
            self.progress
                .retarget(target, motion.adjust_ms(TRANSITION_MS), curve, animate);
            let trace = Some(Trace {
                tween: self.progress,
                samples: Vec::new(),
            });
            if animate {
                if open {
                    self.open_trace = trace;
                } else {
                    self.close_trace = trace;
                }
            }
        } else if !animate {
            self.progress = Tween::settled(target);
        }
        self.progress.step(dt_ms);
        let (t, value) = (self.progress.elapsed_ms, self.progress.value());
        for trace in [&mut self.open_trace, &mut self.close_trace]
            .into_iter()
            .flatten()
        {
            if trace.tween.from == self.progress.from
                && trace.tween.to == self.progress.to
                && trace.tween.curve == self.progress.curve
                && trace.samples.len() < TRACE_SAMPLES
                && trace.samples.last().is_none_or(|(last, _)| *last < t)
                && t <= trace.tween.duration_ms + 50.0
            {
                trace.samples.push((t, value));
            }
        }
    }

    /// Whether a swipe holds the transition.
    pub fn swiping(&self) -> bool {
        self.swipe.is_some()
    }

    /// The swipe has travelled `travel` touchpad units toward open in
    /// all, at `time`, over `distance` units for the whole transition.
    /// The first call begins it where the transition is. The transition
    /// follows the fingers, or with motion off jumps to the nearer end.
    pub fn swipe_update(&mut self, time: u32, travel: f64, distance: f64, motion: MotionPolicy) {
        let animate = motion.allows_motion();
        let at = self.progress.value();
        let swipe = self.swipe.get_or_insert_with(|| Swipe {
            initial: at,
            travel: 0.0,
            history: Default::default(),
        });
        swipe.history.append(time, travel - swipe.travel);
        swipe.travel = travel;
        let raw = (swipe.initial + travel / distance.max(1.0)).clamp(0.0, 1.0);
        let shown = if animate {
            raw
        } else if raw >= 0.5 {
            1.0
        } else {
            0.0
        };
        self.progress = Tween::settled(shown);
    }

    /// The swipe ended at `time`: finish toward where GNOME's tracker
    /// would, carrying the fingers' velocity into an `EASE_OUT_CUBIC`
    /// of matching duration (back where it began when `cancelled`).
    /// Returns whether the overview ends open, or `None` without a swipe.
    pub fn swipe_end(
        &mut self,
        time: u32,
        cancelled: bool,
        distance: f64,
        motion: MotionPolicy,
    ) -> Option<bool> {
        let animate = motion.allows_motion();
        let Swipe {
            initial,
            mut history,
            ..
        } = self.swipe.take()?;
        let at = self.progress.value();
        let velocity = history.velocity(time);
        let (target, duration) = crate::workspace_slide::swipe_release_over(
            at,
            initial.round().clamp(0.0, 1.0),
            2,
            velocity,
            cancelled,
            distance,
        );
        let duration = motion.adjust_ms(duration);
        self.progress = if animate && duration > 0.0 {
            Tween::new(at, target, duration, Curve::EaseOutCubic)
        } else {
            Tween::settled(target)
        };
        self.release = Some(Release {
            from: at,
            target,
            velocity,
            duration_ms: if animate { duration } else { 0.0 },
        });
        Some(target >= 1.0)
    }

    /// A preview drag began: start GNOME's shrink.
    pub fn drag_begin(&mut self, motion: MotionPolicy) {
        self.dragging = true;
        self.drag_shrink = Tween::settled(0.0);
        self.drag_shrink.retarget(
            1.0,
            motion.adjust_ms(DRAG_SCALE_MS),
            Curve::EaseOutQuad,
            motion.allows_motion(),
        );
    }

    /// A drag dropped on nothing: glide the preview home from where it
    /// was drawn.
    pub fn drag_snap_back(&mut self, motion: MotionPolicy) {
        self.drag_released();
        if motion.allows_motion() {
            self.pending = Some(MorphCause::SnapBack);
        }
    }

    /// A drop that changed nothing: the preview reappears in its slot
    /// and fades in over 750 ms.
    /// A fade, so fade-only keeps it.
    pub fn drag_revert(&mut self, id: u64, motion: MotionPolicy) {
        self.drag_released();
        if motion.allows_fades() {
            let duration = motion.adjust_ms(REVERT_MS);
            self.revert = Some((id, Tween::new(0.0, 1.0, duration, Curve::EaseOutQuad)));
        }
    }

    /// A drop that moved the window: the scene's own change animates it.
    pub fn drag_released(&mut self) {
        self.dragging = false;
        self.drag_shrink = Tween::settled(0.0);
    }

    /// Advance the settled scene's motion by `dt_ms`. `base` is the
    /// overview as it would rest (`layout` or `app_grid_layout`), `drag`
    /// the dragged window and its grab point.
    pub fn step_scene(
        &mut self,
        dt_ms: f64,
        base: &OverviewLayout,
        cues: OverviewCues,
        drag: Option<(u64, Point<f64, Logical>)>,
        pointer: Point<f64, Logical>,
    ) {
        if self.progress.value() <= 0.0 && !cues.open {
            self.reset_scene();
            return;
        }
        let motion = cues.motion;
        let animate = motion.allows_motion();
        let shape = SceneShape::of(base, cues.app_grid);
        let cause = self
            .pending
            .take()
            .or_else(|| self.shape.as_ref().and_then(|old| old.cause(&shape)));
        if let (Some(cause), Some(from), true) = (cause, self.last_scene.take(), animate) {
            let (duration, curve) = cause.timing();
            let duration = motion.adjust_ms(duration);
            self.morph = Some((from, Tween::new(0.0, 1.0, duration, curve), cause));
            self.last_morph = Some(cause);
        }
        self.shape = Some(shape);
        self.search.retarget(
            if cues.search { 1.0 } else { 0.0 },
            motion.adjust_ms(SIDE_CONTROLS_MS),
            Curve::EaseOutQuad,
            motion.allows_fades(),
        );
        // GNOME grows the preview under the pointer once settled.
        self.hovered = (self.progress.value() >= 1.0 && drag.is_none())
            .then(|| hovered_at(base, pointer))
            .flatten();
        if let Some(id) = self.hovered {
            if !self.hover.iter().any(|(h, _)| *h == id) {
                self.hover.push((id, Tween::settled(0.0)));
            }
        }
        for (id, tween) in &mut self.hover {
            let target = if Some(*id) == self.hovered { 1.0 } else { 0.0 };
            tween.retarget(
                target,
                motion.adjust_ms(HOVER_MS),
                Curve::EaseOutQuad,
                animate,
            );
            tween.step(dt_ms);
        }
        self.hover
            .retain(|(id, t)| Some(*id) == self.hovered || t.value() > 0.0);
        if drag.is_none() && self.dragging {
            self.drag_released();
        }
        self.search.step(dt_ms);
        self.drag_shrink.step(dt_ms);
        if let Some((_, tween)) = self.revert.as_mut() {
            tween.step(dt_ms);
        }
        if self.revert.as_ref().is_some_and(|(_, t)| t.done()) {
            self.revert = None;
        }
        if let Some((_, tween, _)) = self.morph.as_mut() {
            tween.step(dt_ms);
        }
        if self.morph.as_ref().is_some_and(|(_, t, _)| t.done()) {
            self.morph = None;
        }
        self.last_scene = Some(self.settled_scene(base, drag, pointer));
    }

    /// Forget the scene's motion (the overview closed).
    pub fn reset_scene(&mut self) {
        self.shape = None;
        self.morph = None;
        self.pending = None;
        self.last_scene = None;
        self.search = Tween::settled(0.0);
        self.hovered = None;
        self.hover.clear();
        self.revert = None;
        self.drag_released();
    }

    /// The overview scene before the open/close transition and search
    /// fade: hover, drag, revert and any running morph applied.
    fn settled_scene(
        &self,
        base: &OverviewLayout,
        drag: Option<(u64, Point<f64, Logical>)>,
        pointer: Point<f64, Logical>,
    ) -> OverviewLayout {
        let mut scene = base.clone();
        match drag {
            Some((id, start)) => {
                if let Some(at) = insertion_target(&scene, pointer) {
                    show_placeholder(&mut scene, at);
                }
                drag_preview_shrinking(&mut scene, id, start, pointer, self.drag_shrink.value());
            }
            None => {
                for (id, tween) in &self.hover {
                    grow_preview(&mut scene, *id, tween.value());
                }
                scene.hovered = self.hovered;
            }
        }
        if let Some((id, tween)) = self.revert {
            for p in scene.previews.iter_mut().filter(|p| p.id == id && p.active) {
                p.alpha *= tween.value() as f32;
            }
        }
        if let Some((from, tween, _)) = &self.morph {
            scene = blend(from, &scene, tween.value());
        }
        scene
    }

    /// The overview as drawn this frame from its resting `base`.
    pub fn scene(
        &self,
        base: &OverviewLayout,
        drag: Option<(u64, Point<f64, Logical>)>,
        pointer: Point<f64, Logical>,
        output: Rectangle<i32, Logical>,
        windows: &[OverviewWindow],
    ) -> OverviewLayout {
        let mut scene = self.settled_scene(base, drag, pointer);
        // Search fades the workspaces out (`_onSearchChanged`).
        let shown = 1.0 - self.search.value() as f32;
        if shown < 1.0 {
            for c in scene.cards.iter_mut().chain(scene.thumbnails.iter_mut()) {
                c.alpha *= shown;
            }
            for p in &mut scene.previews {
                p.alpha *= shown;
            }
        }
        let progress = self.progress.value();
        if progress < 1.0 {
            return transition(&scene, progress, output, windows);
        }
        scene
    }

    /// Timings and samples for state snapshots and proofs.
    pub fn diagnostics(&self) -> serde_json::Value {
        let trace = |t: &Option<Trace>| {
            t.as_ref().map(|t| {
                serde_json::json!({
                    "from": t.tween.from,
                    "to": t.tween.to,
                    "curve": t.tween.curve.name(),
                    "duration_ms": t.tween.duration_ms,
                    "samples": t.samples,
                })
            })
        };
        serde_json::json!({
            "open": trace(&self.open_trace),
            "close": trace(&self.close_trace),
            "running": (!self.progress.done()).then(|| serde_json::json!({
                "from": self.progress.from,
                "to": self.progress.to,
                "curve": self.progress.curve.name(),
                "duration_ms": self.progress.duration_ms,
            })),
            "release": self.release.map(|r| serde_json::json!({
                "from": r.from,
                "target": r.target,
                "velocity": r.velocity,
                "duration_ms": r.duration_ms,
                "curve": Curve::EaseOutCubic.name(),
            })),
            "morph": self.morph.as_ref().map(|(_, t, cause)| serde_json::json!({
                "cause": cause.name(),
                "curve": t.curve.name(),
                "duration_ms": t.duration_ms,
                "time": t.time(),
            })),
            "last_morph": self.last_morph.map(|cause| {
                let (duration, curve) = cause.timing();
                serde_json::json!({
                    "cause": cause.name(),
                    "curve": curve.name(),
                    "duration_ms": duration,
                })
            }),
            "search": self.search.value(),
            "hover": self.hover.iter().map(|(id, t)| serde_json::json!([id, t.value()])).collect::<Vec<_>>(),
            "drag_shrink": self.drag_shrink.value(),
            "revert": self.revert.map(|(id, t)| serde_json::json!([id, t.value()])),
        })
    }
}

fn lerp_rect(
    a: Rectangle<i32, Logical>,
    b: Rectangle<i32, Logical>,
    p: f64,
) -> Rectangle<i32, Logical> {
    let l = |x: i32, y: i32| (f64::from(x) + (f64::from(y) - f64::from(x)) * p).round() as i32;
    Rectangle::new(
        (l(a.loc.x, b.loc.x), l(a.loc.y, b.loc.y)).into(),
        (l(a.size.w, b.size.w), l(a.size.h, b.size.h)).into(),
    )
}

/// The overview part-way open (GNOME's transition): at drawn progress
/// `p` (0 the desktop, 1 the overview) the active workspace card grows
/// out of the whole output and each window on it glides from where it
/// sits on the desktop into its preview. Neighbouring cards and the
/// thumbnails slide and fade in with progress; hover growth waits for completion.
pub fn transition(
    layout: &OverviewLayout,
    p: f64,
    output: Rectangle<i32, Logical>,
    windows: &[OverviewWindow],
) -> OverviewLayout {
    if p >= 1.0 {
        return layout.clone();
    }
    let p = p.max(0.0);
    let slide = |card: &WorkspaceCard, thumbnail: bool| {
        let mut start = card.rect;
        if thumbnail {
            start.loc.y = output.loc.y - card.rect.size.h;
        } else if card.rect.loc.x < output.loc.x + output.size.w / 2 {
            start.loc.x = output.loc.x - card.rect.size.w;
        } else {
            start.loc.x = output.loc.x + output.size.w;
        }
        lerp_rect(start, card.rect, p)
    };
    let mut out = OverviewLayout {
        cards: layout
            .cards
            .iter()
            .filter(|c| c.active || p > 0.0)
            .map(|c| WorkspaceCard {
                rect: if c.active {
                    lerp_rect(output, c.rect, p)
                } else {
                    slide(c, false)
                },
                alpha: if c.active {
                    c.alpha
                } else {
                    c.alpha * p as f32
                },
                ..*c
            })
            .collect(),
        thumbnails: layout
            .thumbnails
            .iter()
            .filter(|_| p > 0.0)
            .map(|c| WorkspaceCard {
                rect: slide(c, true),
                alpha: c.alpha * p as f32,
                ..*c
            })
            .collect(),
        ..Default::default()
    };
    for preview in &layout.previews {
        if !preview.active {
            if p == 0.0 {
                continue;
            }
            // Thumbnail clones and neighbouring workspace clones travel with
            // their own card, retaining their scale and stacking order.
            let center = preview.rect.loc
                + Point::<i32, Logical>::from((preview.rect.size.w / 2, preview.rect.size.h / 2));
            let thumbnail = layout.thumbnails.iter().find(|c| c.rect.contains(center));
            let owner = thumbnail.or_else(|| {
                layout
                    .cards
                    .iter()
                    .filter(|c| !c.active)
                    .find(|c| c.rect.contains(center))
            });
            let rect = owner
                .map(|c| {
                    let translated = slide(c, thumbnail.is_some());
                    Rectangle::new(
                        preview.rect.loc + (translated.loc - c.rect.loc),
                        preview.rect.size,
                    )
                })
                .unwrap_or(preview.rect);
            out.previews.push(Preview {
                rect,
                alpha: preview.alpha * p as f32,
                ..*preview
            });
            continue;
        }
        let Some(window) = windows.iter().find(|w| w.id == preview.id) else {
            continue;
        };
        let rect = lerp_rect(window.geometry, preview.rect, p);
        let scale = f64::from(rect.size.w) / f64::from(window.geometry.size.w.max(1));
        out.previews.push(Preview {
            rect,
            scale,
            ..*preview
        });
    }
    out
}

/// What a press at a point means in the overview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewHit {
    /// A window preview on the active card: focus it, close overview.
    Window(u64),
    /// A neighboring workspace card: switch to it.
    Workspace(u32),
    /// Empty space on the active card or the backdrop: close overview.
    Dismiss,
}

/// The card for the workspace `offset` places from the active one: the
/// work area (`output` below `work_top`) scaled down, neighbors one
/// picker pitch away and shrunk about their center.
fn card_rect(
    output: Rectangle<i32, Logical>,
    work_top: i32,
    offset: i32,
    shift: i32,
) -> Rectangle<i32, Logical> {
    let work_h = (output.size.h - work_top).max(1);
    let full_w = (f64::from(output.size.w) * CARD_SCALE).round();
    let full_h = (f64::from(work_h) * CARD_SCALE).round();
    // The thumbnails strip takes `shift` off the top; the card keeps its
    // aspect (GNOME 51: 922x553 becomes 849x509 at y 152).
    let h = (full_h as i32 - shift).max(1);
    let w = (full_w * f64::from(h) / full_h.max(1.0)).round() as i32;
    let x = output.loc.x + (output.size.w - w) / 2;
    let y = output.loc.y + (f64::from(output.size.h) * CARD_TOP).round() as i32 + shift;
    if offset == 0 {
        return Rectangle::new((x, y).into(), (w, h).into());
    }
    let pitch = w + 2 * PICKER_PAD_X + WORKSPACE_SPACING;
    let (cx, cy) = (
        f64::from(x) + f64::from(w) / 2.0 + f64::from(offset * pitch),
        f64::from(y) + f64::from(h) / 2.0,
    );
    let (sw, sh) = (
        (f64::from(w) * INACTIVE_SCALE).round(),
        (f64::from(h) * INACTIVE_SCALE).round(),
    );
    Rectangle::new(
        (
            (cx - sw / 2.0).round() as i32,
            (cy - sh / 2.0).round() as i32,
        )
            .into(),
        (sw as i32, sh as i32).into(),
    )
}

/// Workspaces as the overview shows them: the model's, plus GNOME's
/// trailing empty one after an occupied last workspace, and never fewer
/// than two (`MIN_NUM_WORKSPACES`).
pub fn shown_workspaces(workspaces: &[u32], windows: &[OverviewWindow]) -> Vec<u32> {
    let mut shown = workspaces.to_vec();
    if shown.is_empty() {
        shown.push(0);
    }
    if let Some(&last) = shown.last() {
        if shown.len() < 2 || windows.iter().any(|w| w.workspace == last) {
            shown.push(last + 1);
        }
    }
    shown
}

/// Preview scale cap (`WINDOW_PREVIEW_MAXIMUM_SCALE`).
const MAX_PREVIEW_SCALE: f64 = 0.95;
/// Space between previews: the window picker's 6px plus the preview
/// chrome's largest overhang, the app icon below (64px, 70% overlapping
/// the window) and GNOME's 5px hover growth (`WINDOW_ACTIVE_SIZE_INC`).
const PREVIEW_SPACING: f64 = 6.0 + (1.0 - 0.7) * 64.0 + 5.0;
/// GNOME's layout tradeoff weights (`LAYOUT_SCALE_WEIGHT`, `_SPACE_`).
const LAYOUT_SCALE_WEIGHT: f64 = 1.0;
const LAYOUT_SPACE_WEIGHT: f64 = 0.1;

struct Row {
    windows: Vec<usize>,
    full_width: f64,
    full_height: f64,
}

/// GNOME 51's window spread (workspace.js `UnalignedLayoutStrategy`
/// with `_createBestLayout`), ported arithmetic for arithmetic so ties
/// break the same way: the layout scale is fitted to the work area,
/// then the rows are fitted into `area` (the window picker). Returns
/// each window's on-screen preview and scale.
pub fn window_slots(
    workarea: Rectangle<i32, Logical>,
    monitor_height: i32,
    area: Rectangle<i32, Logical>,
    windows: &[(u64, Rectangle<i32, Logical>)],
) -> Vec<(u64, Rectangle<i32, Logical>, f64)> {
    window_slots_spaced(workarea, monitor_height, area, windows, PREVIEW_SPACING)
}

/// GNOME's screenshot window selector (screenshot.js
/// `UIWindowSelectorLayout`): the same spread, its windows carrying no
/// chrome, so only the 6px selection borders keep them apart.
pub const SELECTOR_SPACING: f64 = 12.0;

/// [`window_slots`] with `spacing` logical pixels between previews.
pub fn window_slots_spaced(
    workarea: Rectangle<i32, Logical>,
    monitor_height: i32,
    area: Rectangle<i32, Logical>,
    windows: &[(u64, Rectangle<i32, Logical>)],
    spacing: f64,
) -> Vec<(u64, Rectangle<i32, Logical>, f64)> {
    if windows.is_empty() {
        return Vec::new();
    }
    // Small windows grow a little: lerp(1.5, 1, height / monitor height).
    let window_scale = |r: &Rectangle<i32, Logical>| {
        let ratio = f64::from(r.size.h) / f64::from(monitor_height.max(1));
        1.5 + (1.0 - 1.5) * ratio
    };
    let center = |r: &Rectangle<i32, Logical>| {
        (
            f64::from(r.loc.x) + f64::from(r.size.w) / 2.0,
            f64::from(r.loc.y) + f64::from(r.size.h) / 2.0,
        )
    };
    let compute_layout = |num_rows: usize| -> Vec<Row> {
        let mut total_width = 0.0;
        for (_, r) in windows {
            total_width += f64::from(r.size.w) * window_scale(r);
        }
        let ideal = total_width / num_rows as f64;
        let mut sorted: Vec<usize> = (0..windows.len()).collect();
        sorted.sort_by(|a, b| {
            center(&windows[*a].1)
                .1
                .partial_cmp(&center(&windows[*b].1).1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut rows = Vec::new();
        let mut idx = 0;
        for i in 0..num_rows {
            let mut row = Row {
                windows: Vec::new(),
                full_width: 0.0,
                full_height: 0.0,
            };
            while idx < sorted.len() {
                let r = &windows[sorted[idx]].1;
                let s = window_scale(r);
                let width = f64::from(r.size.w) * s;
                let height = f64::from(r.size.h) * s;
                row.full_height = row.full_height.max(height);
                let keep = if row.full_width + width <= ideal {
                    true
                } else {
                    let old = row.full_width / ideal;
                    let new = (row.full_width + width) / ideal;
                    (1.0 - new).abs() < (1.0 - old).abs()
                };
                if keep || i == num_rows - 1 {
                    row.windows.push(sorted[idx]);
                    row.full_width += width;
                    idx += 1;
                } else {
                    break;
                }
            }
            rows.push(row);
        }
        for row in &mut rows {
            row.windows.sort_by(|a, b| {
                center(&windows[*a].1)
                    .0
                    .partial_cmp(&center(&windows[*b].1).0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        rows
    };
    let scale_and_space = |rows: &[Row], w: f64, h: f64| {
        let max_row = rows.iter().fold(
            &rows[0],
            |m, r| if r.full_width > m.full_width { r } else { m },
        );
        let grid_width = max_row.full_width;
        let grid_height: f64 = rows.iter().map(|r| r.full_height).sum();
        let hspacing = (max_row.windows.len() as f64 - 1.0) * spacing;
        let vspacing = (rows.len() as f64 - 1.0) * spacing;
        let scale = ((w - hspacing) / grid_width)
            .min((h - vspacing) / grid_height)
            .min(MAX_PREVIEW_SCALE);
        let space = (grid_width * scale + hspacing) * (grid_height * scale + vspacing) / (w * h);
        (scale, space)
    };
    let better = |old_scale: f64, old_space: f64, scale: f64, space: f64| {
        let space_power = (space - old_space) * LAYOUT_SPACE_WEIGHT;
        let scale_power = (scale - old_scale) * LAYOUT_SCALE_WEIGHT;
        if scale > old_scale && space > old_space {
            true
        } else if scale > old_scale {
            scale_power > space_power
        } else if space > old_space {
            space_power > scale_power
        } else {
            false
        }
    };
    let (ww, wh) = (f64::from(workarea.size.w), f64::from(workarea.size.h));
    let mut best: Option<(Vec<Row>, f64, f64)> = None;
    let mut last_columns = usize::MAX;
    for num_rows in 1.. {
        let columns = windows.len().div_ceil(num_rows);
        if columns == last_columns {
            break;
        }
        let rows = compute_layout(num_rows);
        let (scale, space) = scale_and_space(&rows, ww, wh);
        if let Some((_, s, sp)) = &best {
            if !better(*s, *sp, scale, space) {
                break;
            }
        }
        best = Some((rows, scale, space));
        last_columns = columns;
    }
    let Some((rows, scale, _)) = best else {
        return Vec::new();
    };

    // computeWindowSlots: fit the rows into the picker area.
    let (ax, ay, aw, ah) = (
        f64::from(area.loc.x),
        f64::from(area.loc.y),
        f64::from(area.size.w),
        f64::from(area.size.h),
    );
    let row_width = |r: &Row| r.full_width * scale + (r.windows.len() as f64 - 1.0) * spacing;
    let row_height = |r: &Row| r.full_height * scale;
    let height_without: f64 = rows.iter().map(row_height).sum();
    let vertical_spacing = (rows.len() as f64 - 1.0) * spacing;
    let add_v = (1.0f64).min((ah - vertical_spacing) / height_without);
    let mut compensation = 0.0;
    let mut y = 0.0;
    let mut placed = Vec::new();
    for r in &rows {
        let hs = (r.windows.len() as f64 - 1.0) * spacing;
        let without = row_width(r) - hs;
        let add_h = (1.0f64).min((aw - hs) / without);
        let additional = if add_h < add_v {
            compensation += (add_v - add_h) * row_height(r);
            add_h
        } else {
            add_v
        };
        let x = ax + ((aw - (without * additional + hs)).max(0.0)) / 2.0;
        let ry = ay + ((ah - (height_without + vertical_spacing)).max(0.0)) / 2.0 + y;
        y += row_height(r) * additional + spacing;
        placed.push((x, ry, additional));
    }
    compensation /= 2.0;
    let mut out = Vec::new();
    for (r, (row_x, row_y, additional)) in rows.iter().zip(placed) {
        let row_y = row_y + compensation;
        let rh = row_height(r) * additional;
        let mut x = row_x;
        for &i in &r.windows {
            let (id, geo) = windows[i];
            let mut s = scale * window_scale(&geo) * additional;
            let cell_w = f64::from(geo.size.w) * s;
            let cell_h = f64::from(geo.size.h) * s;
            s = s.min(MAX_PREVIEW_SCALE);
            let clone_w = f64::from(geo.size.w) * s;
            let clone_h = f64::from(geo.size.h) * s;
            let clone_x = (x + (cell_w - clone_w) / 2.0).floor();
            let clone_y = if rows.len() == 1 {
                (row_y + (rh - clone_h) / 2.0).floor()
            } else {
                (row_y + rh - cell_h).floor()
            };
            out.push((
                id,
                Rectangle::new(
                    (clone_x as i32, clone_y as i32).into(),
                    (clone_w.round() as i32, clone_h.round() as i32).into(),
                ),
                s,
            ));
            x += cell_w + spacing;
        }
    }
    out
}

/// The overview scene: the active card with its windows spread GNOME's
/// way ([`window_slots`]), neighbors peeking in with theirs in place.
pub fn layout(
    output: Rectangle<i32, Logical>,
    work_top: i32,
    workspaces: &[u32],
    active: u32,
    windows: &[OverviewWindow],
) -> OverviewLayout {
    layout_shown(
        output,
        work_top,
        &shown_workspaces(workspaces, windows),
        active,
        windows,
    )
}

/// [`layout`] over a precomputed workspace list: fixed counts pass the
/// model's list through, dynamic ones the [`shown_workspaces`] view, so
/// the trailing empty workspace never doubles (#337).
pub fn layout_shown(
    output: Rectangle<i32, Logical>,
    work_top: i32,
    workspaces: &[u32],
    active: u32,
    windows: &[OverviewWindow],
) -> OverviewLayout {
    let mut out = OverviewLayout::default();
    let index = workspaces.iter().position(|w| *w == active).unwrap_or(0);
    let neighbors = [
        (index.checked_sub(1), -1),
        (Some(index + 1).filter(|i| *i < workspaces.len()), 1),
    ];
    let work_h = (output.size.h - work_top).max(1);
    let strip = workspaces.len() >= THUMBNAILS_MIN_WORKSPACES;
    let shift = if strip { thumbnails_offset(work_h) } else { 0 };
    if strip {
        thumbnails(&mut out, output, work_top, &workspaces, active, windows);
    }
    let active_card = card_rect(output, work_top, 0, shift);
    for (slot, offset) in neighbors {
        let Some(slot) = slot else { continue };
        let workspace = workspaces[slot];
        let rect = card_rect(output, work_top, offset, shift);
        let scale = f64::from(rect.size.w) / f64::from(output.size.w.max(1));
        out.cards.push(WorkspaceCard {
            workspace,
            rect,
            active: false,
            alpha: 1.0,
            settled: rect.size,
        });
        for w in windows.iter().filter(|w| w.workspace == workspace) {
            let x =
                rect.loc.x + (f64::from(w.geometry.loc.x - output.loc.x) * scale).round() as i32;
            let y = rect.loc.y
                + (f64::from(w.geometry.loc.y - output.loc.y - work_top) * scale).round() as i32;
            let size = (
                (f64::from(w.geometry.size.w) * scale).round() as i32,
                (f64::from(w.geometry.size.h) * scale).round() as i32,
            );
            out.previews.push(Preview {
                id: w.id,
                rect: Rectangle::new((x, y).into(), size.into()),
                scale,
                active: false,
                alpha: 1.0,
            });
        }
    }
    out.cards.insert(
        0,
        WorkspaceCard {
            workspace: active,
            rect: active_card,
            active: true,
            alpha: 1.0,
            settled: active_card.size,
        },
    );
    // GNOME lays the windows out in the window picker: the card plus
    // PICKER_PAD_X / PICKER_PAD_Y around it.
    let picker = Rectangle::new(
        (
            active_card.loc.x - PICKER_PAD_X,
            active_card.loc.y - PICKER_PAD_Y,
        )
            .into(),
        (
            active_card.size.w + 2 * PICKER_PAD_X,
            active_card.size.h + 2 * PICKER_PAD_Y,
        )
            .into(),
    );
    let workarea = Rectangle::new(
        (output.loc.x, output.loc.y + work_top).into(),
        (output.size.w, (output.size.h - work_top).max(1)).into(),
    );
    let mut mine: Vec<(u64, Rectangle<i32, Logical>)> = windows
        .iter()
        .filter(|w| w.workspace == active)
        .map(|w| (w.id, w.geometry))
        .collect();
    // GNOME sorts by stable sequence: creation order (Tuna Desktop ids rise).
    mine.sort_by_key(|(id, _)| *id);
    for (id, rect, scale) in window_slots(workarea, output.size.h, picker, &mine) {
        out.previews.push(Preview {
            id,
            rect,
            scale,
            active: true,
            alpha: 1.0,
        });
    }
    out
}

/// GNOME's thumbnails strip (workspaceThumbnail.js `ThumbnailsBox`):
/// one 0.034-scale thumbnail per shown workspace, 6px apart, centered
/// below the search entry, windows in place on each (43x26 at y 102 on
/// 1280x800). Miniature windows go in as inactive previews.
fn thumbnails(
    out: &mut OverviewLayout,
    output: Rectangle<i32, Logical>,
    work_top: i32,
    workspaces: &[u32],
    active: u32,
    windows: &[OverviewWindow],
) {
    let work_h = (output.size.h - work_top).max(1);
    let (w, h) = thumbnail_size_for(output.size.w, work_h);
    let n = workspaces.len() as i32;
    let total = n * w + (n - 1) * THUMBNAILS_PAD;
    let start = output.loc.x + (output.size.w - total + 1) / 2;
    let top =
        output.loc.y + (f64::from(output.size.h) * THUMBNAILS_TOP).round() as i32 + THUMBNAILS_PAD;
    let scale = f64::from(w) / f64::from(output.size.w.max(1));
    for (i, &workspace) in workspaces.iter().enumerate() {
        let rect = Rectangle::new(
            (start + i as i32 * (w + THUMBNAILS_PAD), top).into(),
            (w, h).into(),
        );
        out.thumbnails.push(WorkspaceCard {
            workspace,
            rect,
            active: workspace == active,
            alpha: 1.0,
            settled: rect.size,
        });
        for win in windows.iter().filter(|w| w.workspace == workspace) {
            let x =
                rect.loc.x + (f64::from(win.geometry.loc.x - output.loc.x) * scale).round() as i32;
            let y = rect.loc.y
                + (f64::from(win.geometry.loc.y - output.loc.y - work_top) * scale).round() as i32;
            out.previews.push(Preview {
                id: win.id,
                rect: Rectangle::new(
                    (x, y).into(),
                    (
                        ((f64::from(win.geometry.size.w) * scale).round() as i32).max(1),
                        ((f64::from(win.geometry.size.h) * scale).round() as i32).max(1),
                    )
                        .into(),
                ),
                scale,
                active: false,
                alpha: 1.0,
            });
        }
    }
}

/// Rows of a rounded rectangle as `(y, x0, x1)` spans (x1 exclusive):
/// solid fills built from rectangles, as the renderer draws decor.
pub fn rounded_rows(rect: Rectangle<i32, Logical>, radius: i32) -> Vec<(i32, i32, i32)> {
    let r = radius.min(rect.size.w / 2).min(rect.size.h / 2).max(0);
    (0..rect.size.h)
        .map(|row| {
            let from_edge = row.min(rect.size.h - 1 - row);
            let inset = if from_edge < r {
                let d = f64::from(r) - f64::from(from_edge) - 0.5;
                (f64::from(r) - (f64::from(r * r) - d * d).max(0.0).sqrt()).round() as i32
            } else {
                0
            };
            (
                rect.loc.y + row,
                rect.loc.x + inset,
                rect.loc.x + rect.size.w - inset,
            )
        })
        .collect()
}

/// One solid color (RGBA, 0..1) over a set of rectangles.
pub type DecorFill = ([f32; 4], Vec<Rectangle<i32, Logical>>);

/// The strip as solid fills, bottom to top: each thumbnail's background
/// (rounded), then the active indicator's ring. RGBA, 0..1.
pub fn thumbnail_decor(layout: &OverviewLayout, accent: [f32; 3]) -> Vec<DecorFill> {
    // `.workspace-thumbnail` background on the dark overview (#46464e);
    // the indicator in GNOME's accent.
    const FILL: [f32; 4] = [70.0 / 255.0, 70.0 / 255.0, 78.0 / 255.0, 1.0];
    let [ar, ag, ab] = accent;
    let accent = [ar, ag, ab, 1.0];
    if layout.thumbnails.is_empty() {
        return Vec::new();
    }
    let span = |(y, x0, x1): (i32, i32, i32)| Rectangle::new((x0, y).into(), (x1 - x0, 1).into());
    let fills = layout
        .thumbnails
        .iter()
        .flat_map(|t| rounded_rows(t.rect, THUMBNAIL_RADIUS))
        .filter(|(_, x0, x1)| x1 > x0)
        .map(span)
        .collect();
    let mut ring = Vec::new();
    if let Some(t) = layout.thumbnails.iter().find(|t| t.active) {
        let b = THUMBNAIL_INDICATOR_BORDER;
        let outer = Rectangle::new(
            (t.rect.loc.x - b, t.rect.loc.y - b).into(),
            (t.rect.size.w + 2 * b, t.rect.size.h + 2 * b).into(),
        );
        let inner = rounded_rows(t.rect, (THUMBNAIL_INDICATOR_RADIUS - b).max(0));
        for (y, x0, x1) in rounded_rows(outer, THUMBNAIL_INDICATOR_RADIUS) {
            match inner.iter().find(|(iy, _, _)| *iy == y) {
                Some(&(_, i0, i1)) => {
                    ring.push(span((y, x0, i0)));
                    ring.push(span((y, i1, x1)));
                }
                None => ring.push(span((y, x0, x1))),
            }
        }
        ring.retain(|r| r.size.w > 0);
    }
    // Decor is painted with GLES clear, so blend into the overview backdrop
    // explicitly (clear does not alpha-blend like surface render elements).
    let alpha = layout.thumbnails[0].alpha;
    let fade = |color: [f32; 4]| {
        let background = [34.0 / 255.0, 34.0 / 255.0, 38.0 / 255.0];
        [
            background[0] + (color[0] - background[0]) * alpha,
            background[1] + (color[1] - background[1]) * alpha,
            background[2] + (color[2] - background[2]) * alpha,
            1.0,
        ]
    };
    let mut decor = vec![(fade(FILL), fills), (fade(accent), ring)];
    if let Some((_, rect)) = layout.placeholder {
        // GNOME's workspace-placeholder.svg: a fading vertical line,
        // a radial halo and a small solid white center. CSS reserves 18px.
        let scale = f64::from(rect.size.h) / 76.0;
        let cx = f64::from(rect.loc.x) + f64::from(rect.size.w) / 2.0;
        let cy = f64::from(rect.loc.y) + f64::from(rect.size.h) / 2.0;
        for y in rect.loc.y..rect.loc.y + rect.size.h {
            for x in rect.loc.x..rect.loc.x + rect.size.w {
                let dx = (f64::from(x) + 0.5 - cx).abs();
                let dy = (f64::from(y) + 0.5 - cy).abs();
                let distance = dx.hypot(dy);
                let line = (scale + 0.5 - dx).clamp(0.0, 1.0)
                    * (1.0 - dy / (38.0 * scale)).clamp(0.0, 1.0)
                    * 0.49375;
                let glow = (1.0 - distance / (13.5 * scale)).clamp(0.0, 1.0) * 0.43125;
                let dot = (4.57692 * scale + 0.5 - distance).clamp(0.0, 1.0);
                let alpha = 1.0 - (1.0 - line) * (1.0 - glow) * (1.0 - dot);
                if alpha > 0.0 {
                    decor.push((
                        fade([
                            34.0 / 255.0 + (1.0 - 34.0 / 255.0) * alpha as f32,
                            34.0 / 255.0 + (1.0 - 34.0 / 255.0) * alpha as f32,
                            38.0 / 255.0 + (1.0 - 38.0 / 255.0) * alpha as f32,
                            1.0,
                        ]),
                        vec![Rectangle::new((x, y).into(), (1, 1).into())],
                    ));
                }
            }
        }
    }
    decor
}

/// Thumbnail scale of the work area in the app grid state.
pub const THUMBNAIL_SCALE: f64 = 0.15;
/// Thumbnail row top as a fraction of the output height.
pub const THUMBNAIL_TOP: f64 = 0.1275;

/// GNOME's app grid state (`ControlsState.APP_GRID`, workspacesView.js
/// fit-all mode): every workspace as a small card along the top, 0.15 of
/// the work area (192x115 at y 102 on 1280x800), slots 24px apart, the
/// row centered, inactive ones at 0.94, windows in place on each.
pub fn app_grid_layout(
    output: Rectangle<i32, Logical>,
    work_top: i32,
    workspaces: &[u32],
    active: u32,
    windows: &[OverviewWindow],
) -> OverviewLayout {
    app_grid_layout_shown(
        output,
        work_top,
        &shown_workspaces(workspaces, windows),
        active,
        windows,
    )
}

/// [`app_grid_layout`] over a precomputed workspace list, as
/// [`layout_shown`] is for [`layout`] (#337).
pub fn app_grid_layout_shown(
    output: Rectangle<i32, Logical>,
    work_top: i32,
    shown: &[u32],
    active: u32,
    windows: &[OverviewWindow],
) -> OverviewLayout {
    let mut out = OverviewLayout::default();
    let work_h = (output.size.h - work_top).max(1);
    let w = (f64::from(output.size.w) * THUMBNAIL_SCALE).round();
    let h = (f64::from(work_h) * THUMBNAIL_SCALE).round();
    let spacing = f64::from(WORKSPACE_SPACING);
    let n = shown.len() as f64;
    let total = n * w + (n - 1.0) * spacing;
    let start = f64::from(output.loc.x) + f64::from(output.size.w) / 2.0 - total / 2.0;
    let top = f64::from(output.loc.y) + (f64::from(output.size.h) * THUMBNAIL_TOP).round();
    for (i, &workspace) in shown.iter().enumerate() {
        let is_active = workspace == active;
        let (cx, cy) = (start + i as f64 * (w + spacing) + w / 2.0, top + h / 2.0);
        let k = if is_active { 1.0 } else { INACTIVE_SCALE };
        let (sw, sh) = ((w * k).round(), (h * k).round());
        let rect = Rectangle::new(
            (
                (cx - sw / 2.0).floor() as i32,
                (cy - sh / 2.0).floor() as i32,
            )
                .into(),
            (sw as i32, sh as i32).into(),
        );
        let card = WorkspaceCard {
            workspace,
            rect,
            active: is_active,
            alpha: 1.0,
            settled: rect.size,
        };
        if is_active {
            out.cards.insert(0, card);
        } else {
            out.cards.push(card);
        }
        let scale = sw / f64::from(output.size.w.max(1));
        for win in windows.iter().filter(|w| w.workspace == workspace) {
            let x =
                rect.loc.x + (f64::from(win.geometry.loc.x - output.loc.x) * scale).round() as i32;
            let y = rect.loc.y
                + (f64::from(win.geometry.loc.y - output.loc.y - work_top) * scale).round() as i32;
            out.previews.push(Preview {
                id: win.id,
                rect: Rectangle::new(
                    (x, y).into(),
                    (
                        (f64::from(win.geometry.size.w) * scale).round() as i32,
                        (f64::from(win.geometry.size.h) * scale).round() as i32,
                    )
                        .into(),
                ),
                scale,
                active: false,
                alpha: 1.0,
            });
        }
    }
    out
}

/// What a press at `pos` hits: the topmost active preview, else a
/// neighboring card, else dismissal.
pub fn hit(layout: &OverviewLayout, pos: Point<f64, Logical>) -> OverviewHit {
    if let Some(p) = layout
        .previews
        .iter()
        .rev()
        .find(|p| p.active && p.rect.to_f64().contains(pos))
    {
        return OverviewHit::Window(p.id);
    }
    if let Some(t) = layout
        .thumbnails
        .iter()
        .find(|t| t.rect.to_f64().contains(pos))
    {
        return OverviewHit::Workspace(t.workspace);
    }
    if let Some(card) = layout
        .cards
        .iter()
        .find(|c| !c.active && c.rect.to_f64().contains(pos))
    {
        return OverviewHit::Workspace(card.workspace);
    }
    OverviewHit::Dismiss
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: MotionPolicy = MotionPolicy {
        level: tuna_shell_control::motion::MotionLevel::Full,
        slowdown_milli: 1000,
    };
    const OFF: MotionPolicy = MotionPolicy {
        level: tuna_shell_control::motion::MotionLevel::Off,
        slowdown_milli: 1000,
    };

    const BLUE: [f32; 3] = [53.0 / 255.0, 132.0 / 255.0, 228.0 / 255.0];

    fn output() -> Rectangle<i32, Logical> {
        Rectangle::new((0, 0).into(), (1280, 800).into())
    }

    fn win(id: u64, ws: u32, x: i32, y: i32, w: i32, h: i32) -> OverviewWindow {
        OverviewWindow {
            id,
            workspace: ws,
            geometry: Rectangle::new((x, y).into(), (w, h).into()),
        }
    }

    #[test]
    fn active_card_matches_gnome_51() {
        let l = layout(output(), 32, &[0], 0, &[]);
        assert_eq!(l.cards.len(), 2, "GNOME keeps two workspaces, even empty");
        // GNOME Shell 51 at 1280x800: workspace-background 922x553 at 179,108.
        assert_eq!(
            l.cards[0].rect,
            Rectangle::new((179, 108).into(), (922, 553).into())
        );
    }

    #[test]
    fn gnomes_empty_next_workspace_peeks_in() {
        // One occupied workspace: GNOME shows the empty one after it,
        // 0.94 scale, at 1192,125 867x520.
        let l = layout(output(), 32, &[0], 0, &[win(1, 0, 100, 100, 640, 420)]);
        assert_eq!(l.cards.len(), 2);
        let next = l.cards.iter().find(|c| !c.active).unwrap();
        assert_eq!(next.workspace, 1);
        // Within a pixel: GNOME rounds through scaled actor transforms.
        let r = next.rect;
        assert!(
            (r.loc.x - 1192).abs() <= 1 && (r.loc.y - 125).abs() <= 1,
            "{r:?}"
        );
        assert_eq!((r.size.w, r.size.h), (867, 520));
        assert_eq!(
            shown_workspaces(&[0, 1], &[win(1, 0, 0, 0, 1, 1)]),
            vec![0, 1]
        );
    }

    #[test]
    fn single_window_is_centered_and_never_past_gnomes_cap() {
        let l = layout(output(), 32, &[0], 0, &[win(1, 0, 190, 32, 900, 768)]);
        let p = l.previews[0];
        assert!(p.scale <= MAX_PREVIEW_SCALE + f64::EPSILON);
        let card = l.cards[0].rect;
        let center = |r: Rectangle<i32, Logical>| (r.loc.x + r.size.w / 2, r.loc.y + r.size.h / 2);
        let (pc, cc) = (center(p.rect), center(card));
        assert!(
            (pc.0 - cc.0).abs() <= 1 && (pc.1 - cc.1).abs() <= 1,
            "{pc:?} vs {cc:?}"
        );
    }

    #[test]
    fn spread_previews_never_overlap_and_stay_in_the_picker() {
        let ws: Vec<OverviewWindow> = (0..5)
            .map(|i| win(i, 0, 40 * i as i32, 40, 800, 600))
            .collect();
        let l = layout(output(), 32, &[0], 0, &ws);
        // GNOME lays previews out in the window picker, the card plus
        // its padding, so they may overhang the card itself.
        let card = l.cards[0].rect;
        let picker = Rectangle::new(
            (card.loc.x - PICKER_PAD_X, card.loc.y - PICKER_PAD_Y).into(),
            (
                card.size.w + 2 * PICKER_PAD_X,
                card.size.h + 2 * PICKER_PAD_Y,
            )
                .into(),
        );
        let active: Vec<_> = l.previews.iter().filter(|p| p.active).collect();
        assert_eq!(active.len(), 5);
        for (i, a) in active.iter().enumerate() {
            assert!(
                picker.contains_rect(a.rect),
                "{:?} inside {:?}",
                a.rect,
                picker
            );
            for b in &active[i + 1..] {
                assert!(
                    !a.rect.overlaps(b.rect),
                    "{:?} overlaps {:?}",
                    a.rect,
                    b.rect
                );
            }
        }
    }

    #[test]
    fn neighbors_peek_from_the_sides() {
        let l = layout(
            output(),
            32,
            &[0, 1, 2],
            1,
            &[win(7, 0, 0, 32, 640, 400), win(8, 2, 0, 32, 640, 400)],
        );
        assert_eq!(l.cards.len(), 3);
        let active = l.cards[0].rect;
        let left = l.cards.iter().find(|c| c.workspace == 0).unwrap().rect;
        let right = l.cards.iter().find(|c| c.workspace == 2).unwrap().rect;
        assert!(left.loc.x + left.size.w < active.loc.x);
        assert!(right.loc.x > active.loc.x + active.size.w);
        // Only the edges of the neighbors are on screen.
        assert!(left.loc.x + left.size.w > 0);
        assert!(right.loc.x < 1280);
        // Two on the neighbor cards, two more in the thumbnails strip.
        assert_eq!(l.previews.iter().filter(|p| !p.active).count(), 4);
    }

    #[test]
    fn thumbnails_strip_matches_gnome_51() {
        // GNOME Shell 51 at 1280x800, three workspaces, a window moved
        // to the second: 43x26 thumbnails at x 570/619/668, y 102; the
        // card shrinks to 849x509 at y 152 and the right peek starts
        // at x 1153.
        let l = layout(
            output(),
            32,
            &[0, 1],
            0,
            &[win(1, 0, 320, 182, 640, 420), win(2, 1, 320, 182, 640, 420)],
        );
        let thumbs: Vec<_> = l.thumbnails.iter().map(|t| (t.workspace, t.rect)).collect();
        assert_eq!(
            thumbs,
            vec![
                (0, Rectangle::new((570, 102).into(), (43, 26).into())),
                (1, Rectangle::new((619, 102).into(), (43, 26).into())),
                (2, Rectangle::new((668, 102).into(), (43, 26).into())),
            ]
        );
        assert!(l.thumbnails[0].active);
        let card = l.cards[0].rect;
        assert_eq!((card.loc.y, card.size.w, card.size.h), (152, 849, 509));
        let right = l.cards.iter().find(|c| c.workspace == 1).unwrap().rect;
        assert!((1152..=1154).contains(&right.loc.x), "{right:?}");
        // A press on a thumbnail switches to its workspace.
        assert_eq!(hit(&l, (690.0, 110.0).into()), OverviewHit::Workspace(2));
    }

    #[test]
    fn a_dragged_preview_shrinks_fades_and_follows_the_pointer() {
        let mut l = layout(output(), 32, &[0], 0, &[win(1, 0, 320, 182, 640, 420)]);
        let before = l.previews.iter().find(|p| p.active).copied().unwrap();
        // Grab the preview's center, then move far away.
        let start = Point::from((
            f64::from(before.rect.loc.x + before.rect.size.w / 2),
            f64::from(before.rect.loc.y + before.rect.size.h / 2),
        ));
        let pos = Point::from((300.0, 500.0));
        drag_preview(&mut l, 1, start, pos);
        let p = *l.previews.last().unwrap();
        assert_eq!(p.id, 1);
        assert!(p.rect.size.w.max(p.rect.size.h) <= WINDOW_DND_SIZE as i32);
        assert!((p.alpha - DRAGGING_WINDOW_OPACITY).abs() < f32::EPSILON);
        assert!(!p.active, "a dragged preview is no click target");
        // The grabbed point (the center) stays under the pointer.
        let center = (
            p.rect.loc.x + p.rect.size.w / 2,
            p.rect.loc.y + p.rect.size.h / 2,
        );
        assert!((center.0 - 300).abs() <= 1 && (center.1 - 500).abs() <= 1);
        // The surface scale shrank with the rect.
        let k = f64::from(p.rect.size.w) / f64::from(before.rect.size.w);
        assert!((p.scale - before.scale * k).abs() < 0.01);
    }

    #[test]
    fn drops_land_on_thumbnails_and_neighbor_cards() {
        let l = layout(
            output(),
            32,
            &[0, 1],
            0,
            &[win(1, 0, 320, 182, 640, 420), win(2, 1, 320, 182, 640, 420)],
        );
        // The third thumbnail is GNOME's trailing empty workspace.
        assert_eq!(drop_target(&l, (690.0, 110.0).into()), Some(2));
        assert_eq!(drop_target(&l, (640.0, 115.0).into()), Some(1));
        // The right neighbor's peek.
        assert_eq!(drop_target(&l, (1200.0, 400.0).into()), Some(1));
        // The active card is no drop target.
        assert_eq!(drop_target(&l, (640.0, 400.0).into()), None);
    }

    #[test]
    fn the_transition_grows_the_card_and_glides_windows_like_gnome() {
        let windows = [win(1, 0, 320, 182, 640, 420)];
        let open = layout(output(), 32, &[0], 0, &windows);
        let target = open.previews.iter().find(|p| p.active).copied().unwrap();
        // Closed: the card is the whole output, the window where it sits.
        let closed = transition(&open, 0.0, output(), &windows);
        assert_eq!(closed.cards.len(), 1);
        assert_eq!(closed.cards[0].rect, output());
        assert_eq!(closed.previews[0].rect, windows[0].geometry);
        assert!((closed.previews[0].scale - 1.0).abs() < 1e-9);
        // Half way: half way between.
        let half = transition(&open, 0.5, output(), &windows);
        let mid_x = (windows[0].geometry.loc.x + target.rect.loc.x) / 2;
        assert!((half.previews[0].rect.loc.x - mid_x).abs() <= 1);
        assert!(half.previews[0].scale < 1.0 && half.previews[0].scale > target.scale);
        // Open: exactly the overview.
        assert_eq!(transition(&open, 1.0, output(), &windows), open);
        assert!((ease_out_quad(0.5) - 0.75).abs() < 1e-9);
        assert_eq!(ease_out_quad(2.0), 1.0);
    }

    #[test]
    fn transition_frames_keep_each_cards_settled_size() {
        let windows = [win(1, 0, 320, 182, 640, 420), win(2, 1, 320, 182, 640, 420)];
        let open = layout(output(), 32, &[0, 1, 2], 0, &windows);
        for card in open.cards.iter().chain(&open.thumbnails) {
            assert_eq!(card.settled, card.rect.size);
        }
        for p in [0.0, 0.3, 0.7] {
            let frame = transition(&open, p, output(), &windows);
            for (drawn, settled) in [
                (&frame.cards, &open.cards),
                (&frame.thumbnails, &open.thumbnails),
            ] {
                for card in drawn {
                    let settled = settled
                        .iter()
                        .find(|c| c.workspace == card.workspace)
                        .unwrap();
                    assert_eq!(card.settled, settled.rect.size);
                }
            }
        }
        // The growing active card changes size; only its settled size is cached.
        let closed = transition(&open, 0.0, output(), &windows);
        assert_ne!(closed.cards[0].rect.size, closed.cards[0].settled);
    }

    #[test]
    fn neighboring_workspaces_and_thumbnails_follow_intermediate_swipe_progress() {
        let windows = [win(1, 0, 320, 182, 640, 420), win(2, 1, 320, 182, 640, 420)];
        let open = layout(output(), 32, &[0, 1], 0, &windows);
        let closed = transition(&open, 0.0, output(), &windows);
        assert!(closed.thumbnails.is_empty());
        assert_eq!(closed.cards.len(), 1);
        let early = transition(&open, 0.25, output(), &windows);
        let half = transition(&open, 0.5, output(), &windows);
        assert_eq!(half.cards.len(), open.cards.len());
        assert_eq!(half.thumbnails.len(), 3);
        assert_eq!(half.thumbnails[0].alpha, 0.5);
        assert!(early.thumbnails[0].rect.loc.y < half.thumbnails[0].rect.loc.y);
        assert!(half.thumbnails[0].rect.loc.y < open.thumbnails[0].rect.loc.y);
        let neighbor = half.cards.iter().find(|c| !c.active).unwrap();
        let destination = open.cards.iter().find(|c| !c.active).unwrap();
        assert_eq!(neighbor.alpha, 0.5);
        assert!(neighbor.rect.loc.x > destination.rect.loc.x);
        let neighbor_preview = half
            .previews
            .iter()
            .find(|p| p.id == 2 && p.rect.loc.x > 1000)
            .unwrap();
        assert_eq!(neighbor_preview.alpha, 0.5);
        let fill = thumbnail_decor(&half, BLUE)[0].0;
        assert!(fill[0] > 34.0 / 255.0 && fill[0] < 70.0 / 255.0);
        assert_eq!(transition(&open, 1.0, output(), &windows), open);
    }

    #[test]
    fn two_workspaces_have_no_strip() {
        let l = layout(output(), 32, &[0], 0, &[win(1, 0, 0, 32, 640, 400)]);
        assert!(l.thumbnails.is_empty());
        assert!(thumbnail_decor(&l, BLUE).is_empty());
        assert_eq!(l.cards[0].rect.loc.y, 108);
    }

    #[test]
    fn thumbnail_decor_rounds_and_rings() {
        let l = layout(output(), 32, &[0, 1], 0, &[win(2, 1, 0, 32, 640, 400)]);
        let decor = thumbnail_decor(&l, BLUE);
        assert_eq!(decor.len(), 2);
        let (_, fills) = &decor[0];
        // Rounded: the top row of the first thumbnail is inset.
        let top = fills
            .iter()
            .find(|r| r.loc.y == 102 && r.loc.x < 600)
            .unwrap();
        assert!(top.loc.x > 570 && top.size.w < 43);
        // The ring spans the indicator (thumbnail grown 3px a side) and
        // leaves the thumbnail itself uncovered.
        let (_, ring) = &decor[1];
        let min_x = ring.iter().map(|r| r.loc.x).min().unwrap();
        let max_x = ring.iter().map(|r| r.loc.x + r.size.w).max().unwrap();
        assert_eq!((min_x, max_x), (567, 616));
        assert!(!ring
            .iter()
            .any(|r| r.loc.y == 115 && r.loc.x < 600 && r.loc.x + r.size.w > 580));
    }

    #[test]
    fn window_spread_matches_gnome_51() {
        // GNOME Shell 51 at 1280x800 with three 640x420 windows: previews
        // at 208,96 / 656,96 / 432,400, about 415x273 each.
        let ws: Vec<OverviewWindow> = (1..=3).map(|i| win(i, 0, 100, 200, 640, 420)).collect();
        let l = layout(output(), 32, &[0], 0, &ws);
        let got: Vec<_> = l
            .previews
            .iter()
            .filter(|p| p.active)
            .map(|p| p.rect)
            .collect();
        let want = [(208, 96), (656, 96), (432, 400)];
        assert_eq!(got.len(), 3);
        for (r, (x, y)) in got.iter().zip(want) {
            assert!(
                (r.loc.x - x).abs() <= 2 && (r.loc.y - y).abs() <= 1,
                "{got:?}"
            );
            assert!(
                (r.size.w - 415).abs() <= 3 && (r.size.h - 273).abs() <= 2,
                "{got:?}"
            );
        }
    }

    #[test]
    fn app_grid_thumbnails_match_gnome_51() {
        // GNOME Shell 51's app grid at 1280x800 with one occupied
        // workspace: 192x115 at 436,102 and the empty one 180x108 at
        // 658,105.
        let l = app_grid_layout(output(), 32, &[0], 0, &[win(1, 0, 100, 100, 640, 420)]);
        assert_eq!(l.cards.len(), 2);
        assert_eq!(
            l.cards[0].rect,
            Rectangle::new((436, 102).into(), (192, 115).into())
        );
        assert_eq!(
            l.cards[1].rect,
            Rectangle::new((658, 105).into(), (180, 108).into())
        );
        assert!(
            l.previews.iter().all(|p| !p.active),
            "thumbnails are not window pickers"
        );
    }

    #[test]
    fn hovered_preview_grows_five_pixels_a_side() {
        let mut l = layout(output(), 32, &[0], 0, &[win(1, 0, 100, 100, 640, 420)]);
        let before = l.previews[0].rect;
        let inside = Point::from((f64::from(before.loc.x + 10), f64::from(before.loc.y + 10)));
        grow_hovered(&mut l, inside);
        assert_eq!(l.hovered, Some(1));
        let after = l.previews[0].rect;
        assert_eq!(
            (after.loc.x, after.loc.y),
            (before.loc.x - 5, before.loc.y - 5)
        );
        assert_eq!(
            (after.size.w, after.size.h),
            (before.size.w + 10, before.size.h + 10)
        );
    }

    #[test]
    fn hits_resolve_window_then_workspace_then_dismiss() {
        let l = layout(output(), 32, &[0, 1], 0, &[win(3, 0, 100, 100, 600, 400)]);
        let p = l.previews.iter().find(|p| p.active).unwrap().rect;
        let inside = Point::from(((p.loc.x + 5) as f64, (p.loc.y + 5) as f64));
        assert_eq!(hit(&l, inside), OverviewHit::Window(3));
        let right = l.cards.iter().find(|c| !c.active).unwrap().rect;
        let on_right = Point::from(((right.loc.x + 10) as f64, (right.loc.y + 10) as f64));
        assert_eq!(hit(&l, on_right), OverviewHit::Workspace(1));
        assert_eq!(hit(&l, Point::from((5.0, 790.0))), OverviewHit::Dismiss);
    }
    #[test]
    fn insertion_shifts_edge_miniatures_once() {
        let mut l = layout(output(), 32, &[0, 1, 2], 0, &[]);
        let first_later = l.thumbnails[1].rect;
        let second_later = l.thumbnails[2].rect;
        let original = Point::from((
            first_later.loc.x + first_later.size.w - 1,
            first_later.loc.y + 1,
        ));
        let step = 18 + THUMBNAILS_PAD;
        assert!(second_later.contains(Point::from((original.x + step, original.y))));
        l.previews.push(Preview {
            id: 99,
            rect: Rectangle::new(original, (1, 1).into()),
            scale: 1.0,
            active: false,
            alpha: 1.0,
        });

        show_placeholder(&mut l, 1);

        assert_eq!(l.previews[0].rect.loc.x, original.x + step);
        assert_eq!(l.thumbnails[1].rect.loc.x, first_later.loc.x + step);
        assert_eq!(l.thumbnails[2].rect.loc.x, second_later.loc.x + step);
    }

    #[test]
    fn window_drag_insertion_gaps_spread_thumbnails() {
        let mut l = layout(
            output(),
            32,
            &[0, 1],
            0,
            &[win(3, 0, 100, 100, 600, 400), win(4, 1, 200, 200, 600, 400)],
        );
        let first = l.thumbnails[0].rect;
        let second = l.thumbnails[1].rect;
        let gap = Point::from((
            f64::from(first.loc.x + first.size.w + 3),
            f64::from(first.loc.y + 10),
        ));
        assert_eq!(insertion_target(&l, gap), Some(1));
        let last = l.thumbnails.last().unwrap();
        let after = (
            f64::from(last.rect.loc.x + last.rect.size.w + 3),
            f64::from(last.rect.loc.y + 10),
        )
            .into();
        assert_eq!(insertion_target(&l, after), Some(last.workspace));
        assert_eq!(
            insertion_target(
                &l,
                (
                    f64::from(first.loc.x + first.size.w / 2),
                    f64::from(first.loc.y + 10)
                )
                    .into()
            ),
            None
        );
        show_placeholder(&mut l, 1);
        assert_eq!(l.placeholder.unwrap().0, 1);
        assert_eq!(l.placeholder.unwrap().1.size.w, 18);
        assert_eq!(l.thumbnails[0].rect.loc.x, first.loc.x);
        assert!(l.thumbnails[1].rect.loc.x > second.loc.x);
        assert!(thumbnail_decor(&l, [0.2, 0.5, 0.8]).len() > 2);
    }

    #[test]
    fn gnome_curves_match_clutter_modes() {
        assert!((ease_out_sine(0.5) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9);
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-9);
        for curve in [Curve::EaseOutQuad, Curve::EaseOutSine, Curve::EaseOutCubic] {
            assert_eq!(curve.apply(0.0), 0.0);
            assert!((curve.apply(1.0) - 1.0).abs() < 1e-12);
            assert!((curve.apply(7.0) - 1.0).abs() < 1e-12);
        }
    }

    fn opened(motion: &mut OverviewMotion) {
        motion.step_progress(0.0, true, FULL);
        motion.step_progress(TRANSITION_MS, true, FULL);
        assert_eq!(motion.progress(), 1.0);
    }

    #[test]
    fn opening_eases_out_sine_and_closing_eases_out_quad() {
        let mut motion = OverviewMotion::default();
        motion.step_progress(0.0, true, FULL);
        motion.step_progress(125.0, true, FULL);
        // GNOME's animateToOverview: EASE_OUT_SINE over 250 ms.
        assert!((motion.progress() - ease_out_sine(0.5)).abs() < 1e-9);
        motion.step_progress(125.0, true, FULL);
        assert_eq!(motion.progress(), 1.0);
        motion.step_progress(0.0, false, FULL);
        motion.step_progress(125.0, false, FULL);
        // animateFromOverview: EASE_OUT_QUAD toward hidden, so half the
        // time leaves a quarter of the overview (not ease-in's 0.75).
        assert!((motion.progress() - 0.25).abs() < 1e-9);
        motion.step_progress(125.0, false, FULL);
        assert_eq!(motion.progress(), 0.0);
        let d = motion.diagnostics();
        assert_eq!(d["open"]["curve"], "ease-out-sine");
        assert_eq!(d["close"]["curve"], "ease-out-quad");
        assert_eq!(d["close"]["duration_ms"], 250.0);
        let samples = d["close"]["samples"].as_array().unwrap();
        assert!(samples.iter().any(|s| s[0] == 125.0 && s[1] == 0.25));
    }

    #[test]
    fn reversing_mid_transition_continues_from_the_drawn_value() {
        let mut motion = OverviewMotion::default();
        motion.step_progress(0.0, true, FULL);
        motion.step_progress(100.0, true, FULL);
        let at = motion.progress();
        motion.step_progress(0.0, false, FULL);
        assert!(
            (motion.progress() - at).abs() < 1e-12,
            "no jump on reversal"
        );
        motion.step_progress(TRANSITION_MS, false, FULL);
        assert_eq!(motion.progress(), 0.0);
    }

    #[test]
    fn disabled_motion_jumps_to_the_end_state() {
        let mut motion = OverviewMotion::default();
        motion.step_progress(1.0, true, OFF);
        assert_eq!(motion.progress(), 1.0);
        motion.step_progress(1.0, false, OFF);
        assert_eq!(motion.progress(), 0.0);
        motion.step_progress(0.0, true, FULL);
        motion.step_progress(50.0, true, FULL);
        motion.snap(true);
        assert_eq!(motion.progress(), 1.0);
    }

    #[test]
    fn swipe_release_projects_velocity_like_gnome() {
        // Slow release just past half: nearest end, at GNOME's base
        // velocity, capped at 400 ms.
        assert_eq!(swipe_release(0.5, 0.0, 0.0, 300.0), (1.0, 400.0));
        assert_eq!(swipe_release(0.3, 0.0, 0.2, 300.0).0, 0.0);
        // A flick upward from closed opens even from a fifth of the way,
        // and the fingers' speed sets the duration: 0.8 / (3 / 300) * 3.
        let (target, duration) = swipe_release(0.2, 0.0, 3.0, 300.0);
        assert_eq!(target, 1.0);
        assert!((duration - 240.0).abs() < 1e-9, "{duration}");
        // Faster is shorter, never under 100 ms.
        assert!(swipe_release(0.2, 0.0, 6.0, 300.0).1 < duration);
        assert_eq!(swipe_release(0.9, 0.0, 30.0, 300.0).1, 100.0);
        // A downward flick from open closes from most of the way open.
        assert_eq!(swipe_release(0.8, 1.0, -3.0, 300.0).0, 0.0);
        // Cancelled: back to the start, whatever the fingers did.
        assert_eq!(
            crate::workspace_slide::swipe_release_over(0.8, 0.0, 2, 3.0, true, 300.0).0,
            0.0
        );
    }

    #[test]
    fn a_released_swipe_keeps_its_momentum() {
        let mut motion = OverviewMotion::default();
        for (i, travel) in [15.0, 45.0, 75.0].into_iter().enumerate() {
            motion.swipe_update(1000 + 10 * i as u32, travel, 300.0, FULL);
        }
        assert!((motion.progress() - 0.25).abs() < 1e-9);
        assert_eq!(motion.swipe_end(1030, false, 300.0, FULL), Some(true));
        let d = motion.diagnostics();
        assert_eq!(d["release"]["curve"], "ease-out-cubic");
        assert!((d["release"]["velocity"].as_f64().unwrap() - 3.0).abs() < 1e-9);
        // 0.75 left at 3 px/ms over 300: 225 ms, eased out-cubic.
        let duration = d["release"]["duration_ms"].as_f64().unwrap();
        assert!((duration - 225.0).abs() < 1e-6, "{duration}");
        motion.step_progress(duration / 2.0, true, FULL);
        assert!((motion.progress() - (0.25 + 0.75 * ease_out_cubic(0.5))).abs() < 1e-9);
        motion.step_progress(duration, true, FULL);
        assert_eq!(motion.progress(), 1.0);
        // Cancelled: back where it began.
        motion.swipe_update(2000, -100.0, 300.0, FULL);
        assert_eq!(motion.swipe_end(2010, true, 300.0, FULL), Some(true));
        // Motion off: the held swipe sits at an end and releases there.
        let mut motion = OverviewMotion::default();
        motion.swipe_update(0, 180.0, 300.0, OFF);
        assert_eq!(motion.progress(), 1.0);
        assert_eq!(motion.swipe_end(10, false, 300.0, OFF), Some(true));
        assert_eq!(motion.progress(), 1.0);
        assert_eq!(motion.swipe_end(20, false, 300.0, OFF), None);
    }

    fn cues(app_grid: bool, search: bool) -> OverviewCues {
        OverviewCues {
            open: true,
            app_grid,
            search,
            motion: FULL,
        }
    }

    const AWAY: Point<f64, Logical> = Point::new(-10.0, -10.0);

    #[test]
    fn the_app_grid_swap_morphs_out_sine_over_250_ms() {
        let windows = [win(1, 0, 320, 182, 640, 420)];
        let picker = layout(output(), 32, &[0], 0, &windows);
        let grid = app_grid_layout(output(), 32, &[0], 0, &windows);
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &picker, cues(false, false), None, AWAY);
        motion.step_scene(0.0, &grid, cues(true, false), None, AWAY);
        assert_eq!(motion.diagnostics()["morph"]["cause"], "app-grid");
        assert_eq!(motion.diagnostics()["morph"]["curve"], "ease-out-sine");
        motion.step_scene(125.0, &grid, cues(true, false), None, AWAY);
        let mid = motion.scene(&grid, None, AWAY, output(), &windows);
        let p = ease_out_sine(0.5);
        let expect = lerp_rect(picker.cards[0].rect, grid.cards[0].rect, p);
        assert_eq!(mid.cards[0].rect, expect);
        let from = picker.previews.iter().find(|p| p.active).unwrap();
        let to = grid.previews.iter().find(|p| p.id == 1).unwrap();
        let shown = mid.previews.iter().find(|p| p.id == 1).unwrap();
        assert_eq!(shown.rect, lerp_rect(from.rect, to.rect, p));
        motion.step_scene(125.0, &grid, cues(true, false), None, AWAY);
        assert_eq!(motion.scene(&grid, None, AWAY, output(), &windows), grid);
    }

    #[test]
    fn a_workspace_switch_scrolls_out_cubic_and_new_windows_scale_in() {
        let windows = [win(1, 0, 320, 182, 640, 420), win(2, 1, 320, 182, 640, 420)];
        let first = layout(output(), 32, &[0, 1], 0, &windows);
        let second = layout(output(), 32, &[0, 1], 1, &windows);
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &first, cues(false, false), None, AWAY);
        motion.step_scene(0.0, &second, cues(false, false), None, AWAY);
        let d = motion.diagnostics();
        assert_eq!(d["morph"]["cause"], "workspace");
        assert_eq!(d["morph"]["curve"], "ease-out-cubic");
        motion.step_scene(125.0, &second, cues(false, false), None, AWAY);
        let mid = motion.scene(&second, None, AWAY, output(), &windows);
        let active = mid.cards.iter().find(|c| c.workspace == 1).unwrap().rect;
        let was = first.cards.iter().find(|c| c.workspace == 1).unwrap().rect;
        assert_eq!(
            active,
            lerp_rect(was, second.cards[0].rect, ease_out_cubic(0.5))
        );

        // A window mapped while the overview is open scales in about
        // its slot's center over 250 ms.
        let more = [windows[0], windows[1], win(3, 1, 0, 32, 640, 420)];
        let grown = layout(output(), 32, &[0, 1], 1, &more);
        motion.step_scene(250.0, &second, cues(false, false), None, AWAY);
        motion.step_scene(0.0, &grown, cues(false, false), None, AWAY);
        assert_eq!(motion.diagnostics()["morph"]["cause"], "windows");
        motion.step_scene(NEW_PREVIEW_MS / 2.0, &grown, cues(false, false), None, AWAY);
        let mid = motion.scene(&grown, None, AWAY, output(), &more);
        let target = grown
            .previews
            .iter()
            .find(|p| p.id == 3 && p.active)
            .unwrap();
        let shown = mid.previews.iter().find(|p| p.id == 3 && p.active).unwrap();
        let k = ease_out_quad(0.5);
        assert!((shown.scale - target.scale * k).abs() < 1e-9);
        let center = |r: Rectangle<i32, Logical>| (r.loc.x + r.size.w / 2, r.loc.y + r.size.h / 2);
        let (a, b) = (center(shown.rect), center(target.rect));
        assert!((a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1);
    }

    #[test]
    fn thumbnails_widen_in_over_200_ms() {
        let one = [win(1, 0, 320, 182, 640, 420)];
        let two = [one[0], win(2, 1, 320, 182, 640, 420)];
        let before = layout(output(), 32, &[0, 1], 0, &one);
        let after = layout(output(), 32, &[0, 1], 0, &two);
        assert!(before.thumbnails.is_empty() && after.thumbnails.len() == 3);
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &before, cues(false, false), None, AWAY);
        motion.step_scene(0.0, &after, cues(false, false), None, AWAY);
        assert_eq!(motion.diagnostics()["morph"]["cause"], "thumbnails");
        assert_eq!(motion.diagnostics()["morph"]["duration_ms"], 200.0);
        motion.step_scene(100.0, &after, cues(false, false), None, AWAY);
        let mid = motion.scene(&after, None, AWAY, output(), &two);
        let full = after.thumbnails[0].rect;
        let shown = mid.thumbnails[0];
        assert!(shown.rect.size.w > 0 && shown.rect.size.w < full.size.w);
        assert!((shown.alpha - 0.75).abs() < 1e-6);
    }

    #[test]
    fn hover_grows_over_200_ms_out_quad() {
        let windows = [win(1, 0, 320, 182, 640, 420)];
        let base = layout(output(), 32, &[0], 0, &windows);
        let rest = base.previews[0];
        let center = Point::from((
            f64::from(rest.rect.loc.x + rest.rect.size.w / 2),
            f64::from(rest.rect.loc.y + rest.rect.size.h / 2),
        ));
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &base, cues(false, false), None, center);
        motion.step_scene(100.0, &base, cues(false, false), None, center);
        let mid = motion.scene(&base, None, center, output(), &windows);
        assert_eq!(mid.hovered, Some(1));
        let g = (f64::from(HOVER_GROWTH) * ease_out_quad(0.5)).round() as i32;
        assert_eq!(mid.previews[0].rect.loc.x, rest.rect.loc.x - g);
        motion.step_scene(100.0, &base, cues(false, false), None, center);
        let mut full = base.clone();
        grow_hovered(&mut full, center);
        assert_eq!(motion.scene(&base, None, center, output(), &windows), full);
        // Leaving shrinks it back over the same time.
        motion.step_scene(100.0, &base, cues(false, false), None, AWAY);
        let leaving = motion.scene(&base, None, AWAY, output(), &windows);
        assert_eq!(leaving.hovered, None);
        assert!(leaving.previews[0].rect.size.w > rest.rect.size.w);
        motion.step_scene(100.0, &base, cues(false, false), None, AWAY);
        assert_eq!(motion.scene(&base, None, AWAY, output(), &windows), base);
    }

    #[test]
    fn drags_shrink_then_snap_back_or_revert_like_gnome() {
        let windows = [win(1, 0, 320, 182, 640, 420)];
        let base = layout(output(), 32, &[0], 0, &windows);
        let rest = base.previews[0];
        let start = Point::from((
            f64::from(rest.rect.loc.x + 10),
            f64::from(rest.rect.loc.y + 10),
        ));
        let pointer = Point::from((100.0, 740.0));
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &base, cues(false, false), None, start);
        motion.drag_begin(FULL);
        motion.step_scene(
            DRAG_SCALE_MS / 2.0,
            &base,
            cues(false, false),
            Some((1, start)),
            pointer,
        );
        let mid = motion.scene(&base, Some((1, start)), pointer, output(), &windows);
        let dragged = *mid.previews.last().unwrap();
        let mut full = base.clone();
        drag_preview(&mut full, 1, start, pointer);
        let shrunk = *full.previews.last().unwrap();
        assert!(dragged.rect.size.w < rest.rect.size.w);
        assert!(dragged.rect.size.w > shrunk.rect.size.w);
        assert_eq!(dragged.alpha, DRAGGING_WINDOW_OPACITY);
        // Dropped on nothing: glide home over 250 ms.
        motion.drag_snap_back(FULL);
        motion.step_scene(0.0, &base, cues(false, false), None, pointer);
        assert_eq!(motion.diagnostics()["morph"]["cause"], "snap-back");
        motion.step_scene(SNAP_BACK_MS / 2.0, &base, cues(false, false), None, pointer);
        let home = motion.scene(&base, None, pointer, output(), &windows);
        let gliding = home.previews.iter().find(|p| p.id == 1).unwrap();
        assert!(gliding.rect.loc.x > dragged.rect.loc.x && gliding.rect.loc.x < rest.rect.loc.x);
        motion.step_scene(SNAP_BACK_MS / 2.0, &base, cues(false, false), None, pointer);
        assert_eq!(motion.scene(&base, None, pointer, output(), &windows), base);
        // Accepted without a change: back in place, fading in for 750 ms.
        motion.drag_revert(1, FULL);
        motion.step_scene(REVERT_MS / 2.0, &base, cues(false, false), None, pointer);
        let fading = motion.scene(&base, None, pointer, output(), &windows);
        assert_eq!(fading.previews[0].rect, rest.rect);
        assert!((fading.previews[0].alpha - 0.75).abs() < 1e-6);
        motion.step_scene(REVERT_MS / 2.0, &base, cues(false, false), None, pointer);
        assert_eq!(motion.scene(&base, None, pointer, output(), &windows), base);
    }

    #[test]
    fn search_fades_the_workspaces_over_250_ms() {
        let windows = [win(1, 0, 320, 182, 640, 420)];
        let base = layout(output(), 32, &[0], 0, &windows);
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &base, cues(false, false), None, AWAY);
        motion.step_scene(125.0, &base, cues(false, true), None, AWAY);
        let mid = motion.scene(&base, None, AWAY, output(), &windows);
        assert!((mid.cards[0].alpha - 0.25).abs() < 1e-6);
        assert!((mid.previews[0].alpha - 0.25).abs() < 1e-6);
        assert!(!motion.search_covers());
        motion.step_scene(125.0, &base, cues(false, true), None, AWAY);
        assert!(motion.search_covers());
        // Motion off: no tween anywhere.
        let mut off = OverviewMotion::default();
        off.step_progress(0.0, true, OFF);
        let still = OverviewCues {
            motion: OFF,
            ..cues(false, true)
        };
        off.step_scene(0.0, &base, still, None, AWAY);
        assert!(off.search_covers());
    }

    #[test]
    fn fade_only_keeps_fades_and_slow_down_stretches_every_tween() {
        let windows = [win(1, 0, 320, 182, 640, 420)];
        let base = layout(output(), 32, &[0], 0, &windows);
        let fade_only = MotionPolicy::new(tuna_shell_control::motion::MotionLevel::FadeOnly, 1.0);
        let mut motion = OverviewMotion::default();
        // The transition moves and scales: fade-only snaps it.
        motion.step_progress(0.0, true, fade_only);
        assert_eq!(motion.progress(), 1.0);
        let only_fades = OverviewCues {
            motion: fade_only,
            ..cues(false, false)
        };
        motion.step_scene(0.0, &base, only_fades, None, AWAY);
        // Search is a fade: it still takes its 250 ms.
        let searching = OverviewCues {
            search: true,
            ..only_fades
        };
        motion.step_scene(125.0, &base, searching, None, AWAY);
        assert!((motion.diagnostics()["search"].as_f64().unwrap() - 0.75).abs() < 1e-9);
        // GNOME's slow-down factor scales durations (`adjustAnimationTime`).
        let slow = MotionPolicy::new(tuna_shell_control::motion::MotionLevel::Full, 2.0);
        let mut motion = OverviewMotion::default();
        motion.step_progress(0.0, true, slow);
        motion.step_progress(250.0, true, slow);
        assert!((motion.progress() - ease_out_sine(0.5)).abs() < 1e-9);
        assert_eq!(motion.diagnostics()["open"]["duration_ms"], 500.0);
    }

    #[test]
    fn fading_overview_elements_are_never_drawn_opaque() {
        // Occlusion culling (#503) trusts `opaque_regions`, which smithay
        // empties below alpha 1: every element part-way through a fade
        // must carry that alpha so nothing beneath it is culled.
        let windows = [win(1, 0, 320, 182, 640, 420), win(2, 1, 320, 182, 640, 420)];
        let open = layout(output(), 32, &[0, 1], 0, &windows);
        let mid = transition(&open, 0.4, output(), &windows);
        for card in mid
            .cards
            .iter()
            .filter(|c| !c.active)
            .chain(&mid.thumbnails)
        {
            assert!(card.alpha < 1.0, "{card:?}");
        }
        assert!(mid
            .previews
            .iter()
            .filter(|p| !p.active)
            .all(|p| p.alpha < 1.0));
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &open, cues(false, false), None, AWAY);
        motion.step_scene(60.0, &open, cues(false, true), None, AWAY);
        let searching = motion.scene(&open, None, AWAY, output(), &windows);
        assert!(searching.cards.iter().all(|c| c.alpha < 1.0));
        assert!(searching.thumbnails.iter().all(|c| c.alpha < 1.0));
        assert!(searching.previews.iter().all(|p| p.alpha < 1.0));
        // Leaving a morph: what goes fades, never opaque on its way out.
        let fewer = layout(output(), 32, &[0], 0, &windows[..1]);
        let mut motion = OverviewMotion::default();
        opened(&mut motion);
        motion.step_scene(0.0, &open, cues(false, false), None, AWAY);
        motion.step_scene(0.0, &fewer, cues(false, false), None, AWAY);
        motion.step_scene(50.0, &fewer, cues(false, false), None, AWAY);
        let leaving = motion.scene(&fewer, None, AWAY, output(), &windows);
        assert!(leaving.thumbnails.iter().all(|t| t.alpha < 1.0));
        assert!(leaving
            .previews
            .iter()
            .filter(|p| !fewer
                .previews
                .iter()
                .any(|f| f.id == p.id && f.active == p.active))
            .all(|p| p.alpha < 1.0));
    }
}
