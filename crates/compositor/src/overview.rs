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

use smithay::utils::{Logical, Point, Rectangle};

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
}

/// GNOME's hover growth (`WINDOW_ACTIVE_SIZE_INC`): 5px each side.
pub const HOVER_GROWTH: i32 = 5;

/// Grow the topmost active preview under `pointer` by
/// [`HOVER_GROWTH`] a side, as GNOME does, and remember it as hovered.
pub fn grow_hovered(layout: &mut OverviewLayout, pointer: Point<f64, Logical>) {
    let Some(p) = layout
        .previews
        .iter_mut()
        .rev()
        .find(|p| p.active && p.rect.to_f64().contains(pointer))
    else {
        return;
    };
    let g = HOVER_GROWTH;
    let w = p.rect.size.w.max(1);
    p.scale *= f64::from(w + 2 * g) / f64::from(w);
    p.rect = Rectangle::new(
        (p.rect.loc.x - g, p.rect.loc.y - g).into(),
        (p.rect.size.w + 2 * g, p.rect.size.h + 2 * g).into(),
    );
    layout.hovered = Some(p.id);
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
) -> Rectangle<i32, Logical> {
    let work_h = (output.size.h - work_top).max(1);
    let w = (f64::from(output.size.w) * CARD_SCALE).round() as i32;
    let h = (f64::from(work_h) * CARD_SCALE).round() as i32;
    let x = output.loc.x + (output.size.w - w) / 2;
    let y = output.loc.y + (f64::from(output.size.h) * CARD_TOP).round() as i32;
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
    if windows.is_empty() {
        return Vec::new();
    }
    let spacing = PREVIEW_SPACING;
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
    let mut out = OverviewLayout::default();
    let workspaces = shown_workspaces(workspaces, windows);
    let index = workspaces.iter().position(|w| *w == active).unwrap_or(0);
    let neighbors = [
        (index.checked_sub(1), -1),
        (Some(index + 1).filter(|i| *i < workspaces.len()), 1),
    ];
    let active_card = card_rect(output, work_top, 0);
    for (slot, offset) in neighbors {
        let Some(slot) = slot else { continue };
        let workspace = workspaces[slot];
        let rect = card_rect(output, work_top, offset);
        let scale = f64::from(rect.size.w) / f64::from(output.size.w.max(1));
        out.cards.push(WorkspaceCard {
            workspace,
            rect,
            active: false,
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
            });
        }
    }
    out.cards.insert(
        0,
        WorkspaceCard {
            workspace: active,
            rect: active_card,
            active: true,
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
    // GNOME sorts by stable sequence: creation order (Roost ids rise).
    mine.sort_by_key(|(id, _)| *id);
    for (id, rect, scale) in window_slots(workarea, output.size.h, picker, &mine) {
        out.previews.push(Preview {
            id,
            rect,
            scale,
            active: true,
        });
    }
    out
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
    let mut out = OverviewLayout::default();
    let shown = shown_workspaces(workspaces, windows);
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
        assert_eq!(l.previews.iter().filter(|p| !p.active).count(), 2);
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
}
