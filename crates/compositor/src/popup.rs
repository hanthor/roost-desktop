//! xdg popups: menus, context menus, combo boxes, popovers (#88).
//!
//! Smithay's [`PopupManager`] keeps the popup tree per parent surface
//! (an xdg toplevel or a layer surface). This module turns that tree
//! into placed rectangles in the global space, so the renderer can draw
//! each popup above its parent and the window manager can hit-test
//! popups before anything else. Placement follows Smithay's own
//! convention: a popup's window geometry sits at the parent's window
//! geometry origin plus the tree offset, and its surface origin is that
//! point minus the popup's own window-geometry offset.
//!
//! Hit-testing uses the configured popup rectangle (positioner size),
//! not buffer contents, so it is deterministic before the first buffer
//! and matches what the client was told.

use smithay::desktop::{PopupKind, PopupManager};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{SurfaceCachedState, XdgPopupSurfaceData};

/// One popup placed in the global space.
#[derive(Debug, Clone)]
pub struct PlacedPopup {
    /// The popup's own surface.
    pub surface: WlSurface,
    /// Where the popup's surface origin (buffer 0,0) lands.
    pub origin: Point<i32, Logical>,
    /// The popup's window-geometry rectangle: what it covers.
    pub rect: Rectangle<i32, Logical>,
}

/// xdg window-geometry origin of `surface` within its buffer (client
/// shadows push it away from 0,0); zero for surfaces without one.
pub fn window_geometry_loc(surface: &WlSurface) -> Point<i32, Logical> {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<SurfaceCachedState>()
            .current()
            .geometry
            .map(|g| g.loc)
            .unwrap_or_default()
    })
}

fn configured_size(popup: &PopupKind) -> smithay::utils::Size<i32, Logical> {
    match popup {
        PopupKind::Xdg(_) => with_states(popup.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgPopupSurfaceData>()
                .map(|data| data.lock().unwrap().current.geometry.size)
                .unwrap_or_default()
        }),
        PopupKind::InputMethod(_) => popup.geometry().size,
    }
}

/// Every live popup of `parent`, parents before children (bottom to
/// top), placed for a parent whose surface origin is `parent_origin`.
/// `geometry_relative` is true for xdg toplevels (positioners are
/// relative to the parent's window geometry) and false for layer
/// surfaces (relative to the surface itself).
pub fn placed_popups(
    parent: &WlSurface,
    parent_origin: Point<i32, Logical>,
    geometry_relative: bool,
) -> Vec<PlacedPopup> {
    let base = if geometry_relative {
        parent_origin + window_geometry_loc(parent)
    } else {
        parent_origin
    };
    PopupManager::popups_for_surface(parent)
        .map(|(popup, offset)| {
            let at = base + offset;
            PlacedPopup {
                surface: popup.wl_surface().clone(),
                origin: at - popup.geometry().loc,
                rect: Rectangle::new(at, configured_size(&popup)),
            }
        })
        .collect()
}

/// Topmost popup containing `pos` among `placed` (bottom-to-top order).
pub fn popup_at(placed: &[PlacedPopup], pos: Point<f64, Logical>) -> Option<&PlacedPopup> {
    placed.iter().rev().find(|p| p.rect.to_f64().contains(pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topmost_popup_wins_and_misses_are_none() {
        // Built without live surfaces: only rects matter for the search.
        let rects: Vec<Rectangle<i32, Logical>> = vec![
            Rectangle::new((0, 0).into(), (100, 100).into()),
            Rectangle::new((50, 50).into(), (100, 100).into()),
        ];
        let pick = |pos: (f64, f64)| {
            rects
                .iter()
                .rposition(|r| r.to_f64().contains(Point::<f64, Logical>::from(pos)))
        };
        assert_eq!(pick((10.0, 10.0)), Some(0));
        assert_eq!(pick((60.0, 60.0)), Some(1), "later (child) popup is on top");
        assert_eq!(pick((500.0, 500.0)), None);
    }
}
