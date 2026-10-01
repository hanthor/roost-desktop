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
//! Proportions are measured from the GNOME 51 baseline frame
//! (`B-OV-OPEN`, 1280x800): card scale 0.72, card top 108.

use smithay::utils::{Logical, Point, Rectangle, Size};

/// Card size as a fraction of the output (GNOME 51: 922x576 on 1280x800).
pub const CARD_SCALE: f64 = 0.72;
/// Card top edge as a fraction of the output height (search sits above).
pub const CARD_TOP: f64 = 0.135;
/// Horizontal gap between neighboring workspace cards.
pub const CARD_GAP: i32 = 92;
/// Padding inside a card around the spread windows.
pub const SPREAD_PADDING: i32 = 24;
/// Gap between spread window previews.
pub const SPREAD_GAP: i32 = 24;

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
    /// Workspace id.
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

fn card_rect(output: Rectangle<i32, Logical>, offset: i32) -> Rectangle<i32, Logical> {
    let w = (f64::from(output.size.w) * CARD_SCALE).round() as i32;
    let h = (f64::from(output.size.h) * CARD_SCALE).round() as i32;
    let x = output.loc.x + (output.size.w - w) / 2 + offset * (w + CARD_GAP);
    let y = output.loc.y + (f64::from(output.size.h) * CARD_TOP).round() as i32;
    Rectangle::new((x, y).into(), (w, h).into())
}

/// Spread `windows` inside `area` on a grid that maximizes their scale,
/// never enlarging a window past `cap`. Grid order follows the input
/// (stacking) order; each window is centered in its cell.
pub fn spread(
    area: Rectangle<i32, Logical>,
    windows: &[(u64, Size<i32, Logical>)],
    cap: f64,
) -> Vec<(u64, Rectangle<i32, Logical>, f64)> {
    let n = windows.len();
    if n == 0 || area.size.w <= 0 || area.size.h <= 0 {
        return Vec::new();
    }
    // Try every column count; keep the grid whose smallest scale is
    // largest (fairest use of space).
    let mut best: Option<(f64, usize)> = None;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let cell_w = (area.size.w - SPREAD_GAP * (cols as i32 - 1)) as f64 / cols as f64;
        let cell_h = (area.size.h - SPREAD_GAP * (rows as i32 - 1)) as f64 / rows as f64;
        if cell_w <= 0.0 || cell_h <= 0.0 {
            continue;
        }
        let worst = windows
            .iter()
            .map(|(_, s)| {
                (cell_w / f64::from(s.w.max(1)))
                    .min(cell_h / f64::from(s.h.max(1)))
                    .min(cap)
            })
            .fold(f64::INFINITY, f64::min);
        if best.is_none_or(|(b, _)| worst > b) {
            best = Some((worst, cols));
        }
    }
    let Some((_, cols)) = best else {
        return Vec::new();
    };
    let rows = n.div_ceil(cols);
    let cell_w = (area.size.w - SPREAD_GAP * (cols as i32 - 1)) as f64 / cols as f64;
    let cell_h = (area.size.h - SPREAD_GAP * (rows as i32 - 1)) as f64 / rows as f64;
    windows
        .iter()
        .enumerate()
        .map(|(i, (id, size))| {
            let (col, row) = (i % cols, i / cols);
            // Center a short last row, as GNOME does.
            let in_row = if row == rows - 1 {
                n - row * cols
            } else {
                cols
            };
            let row_offset = (cols - in_row) as f64 * (cell_w + f64::from(SPREAD_GAP)) / 2.0;
            let scale = (cell_w / f64::from(size.w.max(1)))
                .min(cell_h / f64::from(size.h.max(1)))
                .min(cap);
            let w = (f64::from(size.w) * scale).round() as i32;
            let h = (f64::from(size.h) * scale).round() as i32;
            let cx = f64::from(area.loc.x)
                + row_offset
                + col as f64 * (cell_w + f64::from(SPREAD_GAP))
                + cell_w / 2.0;
            let cy = f64::from(area.loc.y)
                + row as f64 * (cell_h + f64::from(SPREAD_GAP))
                + cell_h / 2.0;
            let rect = Rectangle::new(
                (
                    (cx - f64::from(w) / 2.0).round() as i32,
                    (cy - f64::from(h) / 2.0).round() as i32,
                )
                    .into(),
                (w, h).into(),
            );
            (*id, rect, scale)
        })
        .collect()
}

/// Lay out the overview on `output`: the active workspace's card in the
/// center with its windows spread, and the previous and next workspaces'
/// cards to the sides with their windows at their desktop positions.
pub fn layout(
    output: Rectangle<i32, Logical>,
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
    let active_card = card_rect(output, 0);
    for (slot, offset) in neighbors {
        let Some(slot) = slot else { continue };
        let workspace = workspaces[slot];
        let rect = card_rect(output, offset);
        out.cards.push(WorkspaceCard {
            workspace,
            rect,
            active: false,
        });
        for w in windows.iter().filter(|w| w.workspace == workspace) {
            let x = rect.loc.x
                + (f64::from(w.geometry.loc.x - output.loc.x) * CARD_SCALE).round() as i32;
            let y = rect.loc.y
                + (f64::from(w.geometry.loc.y - output.loc.y) * CARD_SCALE).round() as i32;
            let size = (
                (f64::from(w.geometry.size.w) * CARD_SCALE).round() as i32,
                (f64::from(w.geometry.size.h) * CARD_SCALE).round() as i32,
            );
            out.previews.push(Preview {
                id: w.id,
                rect: Rectangle::new((x, y).into(), size.into()),
                scale: CARD_SCALE,
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
    let inner = Rectangle::new(
        (
            active_card.loc.x + SPREAD_PADDING,
            active_card.loc.y + SPREAD_PADDING,
        )
            .into(),
        (
            active_card.size.w - 2 * SPREAD_PADDING,
            active_card.size.h - 2 * SPREAD_PADDING,
        )
            .into(),
    );
    let mine: Vec<(u64, Size<i32, Logical>)> = windows
        .iter()
        .filter(|w| w.workspace == active)
        .map(|w| (w.id, w.geometry.size))
        .collect();
    for (id, rect, scale) in spread(inner, &mine, CARD_SCALE) {
        out.previews.push(Preview {
            id,
            rect,
            scale,
            active: true,
        });
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
    fn active_card_matches_the_gnome_51_baseline() {
        let l = layout(output(), &[0], 0, &[]);
        assert_eq!(l.cards.len(), 1);
        // B-OV-OPEN: card spans x 179..1101, top 108.
        assert_eq!(
            l.cards[0].rect,
            Rectangle::new((179, 108).into(), (922, 576).into())
        );
    }

    #[test]
    fn single_window_is_centered_and_never_enlarged_past_the_card_scale() {
        let l = layout(output(), &[0], 0, &[win(1, 0, 190, 32, 900, 768)]);
        let p = l.previews[0];
        assert!(p.scale <= CARD_SCALE + f64::EPSILON);
        let card = l.cards[0].rect;
        let center = |r: Rectangle<i32, Logical>| (r.loc.x + r.size.w / 2, r.loc.y + r.size.h / 2);
        let (pc, cc) = (center(p.rect), center(card));
        assert!(
            (pc.0 - cc.0).abs() <= 1 && (pc.1 - cc.1).abs() <= 1,
            "{pc:?} vs {cc:?}"
        );
    }

    #[test]
    fn spread_previews_never_overlap_and_stay_in_the_card() {
        let ws: Vec<OverviewWindow> = (0..5)
            .map(|i| win(i, 0, 40 * i as i32, 40, 800, 600))
            .collect();
        let l = layout(output(), &[0], 0, &ws);
        let card = l.cards[0].rect;
        let active: Vec<_> = l.previews.iter().filter(|p| p.active).collect();
        assert_eq!(active.len(), 5);
        for (i, a) in active.iter().enumerate() {
            assert!(card.contains_rect(a.rect), "{:?} inside {:?}", a.rect, card);
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
    fn hits_resolve_window_then_workspace_then_dismiss() {
        let l = layout(output(), &[0, 1], 0, &[win(3, 0, 100, 100, 600, 400)]);
        let p = l.previews.iter().find(|p| p.active).unwrap().rect;
        let inside = Point::from(((p.loc.x + 5) as f64, (p.loc.y + 5) as f64));
        assert_eq!(hit(&l, inside), OverviewHit::Window(3));
        let right = l.cards.iter().find(|c| !c.active).unwrap().rect;
        let on_right = Point::from(((right.loc.x + 10) as f64, (right.loc.y + 10) as f64));
        assert_eq!(hit(&l, on_right), OverviewHit::Workspace(1));
        assert_eq!(hit(&l, Point::from((5.0, 790.0))), OverviewHit::Dismiss);
    }
}
