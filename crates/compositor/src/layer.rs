//! Layer-shell server state for the supervised panel (001 T4).
//!
//! Advertises `zwlr_layer_shell_v1` so the shell-host panel can attach its
//! top-anchored Activities surface with an exclusive zone (001 spec R3).
//! The server keeps a small [`PanelSurface`] record per mapped layer
//! surface — namespace, layer, and whether the client has acknowledged a
//! configure — and sends the initial configure on map. It never draws or
//! positions anything here; placement and rendering belong to later
//! tasks. Behavioral reference: cosmic-comp's layer-shell handler (record
//! on map, configure on map) with original code throughout.

use smithay::{
    delegate_layer_shell,
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Rectangle, Size},
    wayland::{
        compositor,
        shell::wlr_layer::{
            Anchor, ExclusiveZone, KeyboardInteractivity, LayerSurface, LayerSurfaceCachedState,
            LayerSurfaceConfigure, WlrLayerShellHandler, WlrLayerShellState,
        },
    },
};

use crate::State;

/// The layer a [`PanelSurface`] was created on.
///
/// Re-exported because `PanelSurface::layer` is public: without this a
/// caller cannot name the type of a field it can read, and every consumer
/// (including the shell-host tests) has to declare smithay just to spell
/// `Layer::Overlay`.
pub use smithay::wayland::shell::wlr_layer::Layer;

/// Layer-shell namespace and contract constants.
///
/// Shared with the shell-host: both the compositor and shell read these
/// from the same source to prevent drift.
pub use roost_shell_control::{
    BANNER_NAMESPACE, OVERVIEW_NAMESPACE, PANEL_HEIGHT, PANEL_NAMESPACE,
};

/// One layer surface's placement request, as committed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerRequest {
    /// Anchored edges.
    pub anchor: Anchor,
    /// Requested size (0 on an axis means "stretch between anchors").
    pub size: Size<i32, Logical>,
    /// Margins: top, right, bottom, left.
    pub margin: (i32, i32, i32, i32),
    /// Exclusive-zone request.
    pub zone: ExclusiveZone,
}

impl LayerRequest {
    /// Read the committed request of a layer surface.
    pub fn of(surface: &WlSurface) -> Self {
        compositor::with_states(surface, |states| {
            let mut cached = states.cached_state.get::<LayerSurfaceCachedState>();
            let c = cached.current();
            Self {
                anchor: c.anchor,
                size: c.size,
                margin: (c.margin.top, c.margin.right, c.margin.bottom, c.margin.left),
                zone: c.exclusive_zone,
            }
        })
    }
}

/// The single edge an exclusive zone applies to: anchored to exactly one
/// edge, or to one edge and both perpendicular ones (layer-shell rule).
fn exclusive_edge(anchor: Anchor) -> Option<Anchor> {
    let horiz = anchor.contains(Anchor::LEFT) && anchor.contains(Anchor::RIGHT);
    let vert = anchor.contains(Anchor::TOP) && anchor.contains(Anchor::BOTTOM);
    let only = |edge: Anchor| anchor == edge;
    if only(Anchor::TOP) || anchor == Anchor::TOP | Anchor::LEFT | Anchor::RIGHT {
        Some(Anchor::TOP)
    } else if only(Anchor::BOTTOM) || anchor == Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT {
        Some(Anchor::BOTTOM)
    } else if only(Anchor::LEFT) || anchor == Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM {
        Some(Anchor::LEFT)
    } else if only(Anchor::RIGHT) || anchor == Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM {
        Some(Anchor::RIGHT)
    } else {
        let _ = (horiz, vert);
        None
    }
}

/// Place one request inside `bounds`: stretch between opposite anchors
/// (minus margins) where the size is 0 or both are anchored, else use
/// the requested size; align to the anchored edge (plus its margin) or
/// center on that axis.
fn place(bounds: Rectangle<i32, Logical>, req: &LayerRequest) -> Rectangle<i32, Logical> {
    let (mt, mr, mb, ml) = req.margin;
    let (l, r, t, b) = (
        req.anchor.contains(Anchor::LEFT),
        req.anchor.contains(Anchor::RIGHT),
        req.anchor.contains(Anchor::TOP),
        req.anchor.contains(Anchor::BOTTOM),
    );
    let w = if l && r && req.size.w == 0 {
        bounds.size.w - ml - mr
    } else {
        req.size.w
    }
    .max(0);
    let h = if t && b && req.size.h == 0 {
        bounds.size.h - mt - mb
    } else {
        req.size.h
    }
    .max(0);
    let x = if l {
        bounds.loc.x + ml
    } else if r {
        bounds.loc.x + bounds.size.w - w - mr
    } else {
        bounds.loc.x + (bounds.size.w - w) / 2
    };
    let y = if t {
        bounds.loc.y + mt
    } else if b {
        bounds.loc.y + bounds.size.h - h - mb
    } else {
        bounds.loc.y + (bounds.size.h - h) / 2
    };
    Rectangle::new((x, y).into(), (w, h).into())
}

/// Arrange the layer surfaces of one output (wlr layer-shell semantics):
/// surfaces with a positive exclusive zone are placed first, in order,
/// each reserving its zone plus margin along its edge; neutral surfaces
/// are then placed inside the remaining area; `DontCare` surfaces use the
/// whole output. Returns one rectangle per request, in input order.
pub fn arrange(
    output: Rectangle<i32, Logical>,
    reqs: &[LayerRequest],
) -> Vec<Rectangle<i32, Logical>> {
    let mut usable = output;
    let mut out = vec![Rectangle::default(); reqs.len()];
    for (i, req) in reqs.iter().enumerate() {
        let ExclusiveZone::Exclusive(zone) = req.zone else {
            continue;
        };
        let Some(edge) = exclusive_edge(req.anchor) else {
            continue;
        };
        out[i] = place(usable, req);
        let (mt, mr, mb, ml) = req.margin;
        let zone = zone as i32;
        if edge == Anchor::TOP {
            let cut = zone + mt;
            usable.loc.y += cut;
            usable.size.h -= cut;
        } else if edge == Anchor::BOTTOM {
            usable.size.h -= zone + mb;
        } else if edge == Anchor::LEFT {
            let cut = zone + ml;
            usable.loc.x += cut;
            usable.size.w -= cut;
        } else {
            usable.size.w -= zone + mr;
        }
    }
    for (i, req) in reqs.iter().enumerate() {
        let exclusive =
            matches!(req.zone, ExclusiveZone::Exclusive(_)) && exclusive_edge(req.anchor).is_some();
        if exclusive {
            continue;
        }
        let bounds = if matches!(req.zone, ExclusiveZone::DontCare) {
            output
        } else {
            usable
        };
        out[i] = place(bounds, req);
    }
    out
}

/// Arranged size of one surface against an output of `output` size
/// with no other layer surfaces (kept for callers that size a lone
/// surface; the runtime arranges per output with [`arrange`]).
pub fn arranged_size(surface: &LayerSurface, output: Size<i32, Logical>) -> Size<i32, Logical> {
    let req = LayerRequest::of(surface.wl_surface());
    arrange(Rectangle::from_size(output), &[req])[0].size
}

/// Every mapped layer surface of one output key, in creation order, with
/// its request.
/// Layer surfaces of one output: `(output key, [(surface, request, layer)])`.
type OutputGroup = (Option<String>, Vec<(WlSurface, LayerRequest, Layer)>);

fn output_requests(state: &State) -> Vec<OutputGroup> {
    let mut groups: Vec<OutputGroup> = Vec::new();
    for record in &state.panel_surfaces {
        // Explicit output bindings never fall back to another output merely
        // because a retired client has not destroyed its closed role yet.
        if record
            .output_name
            .as_ref()
            .is_some_and(|name| !state.outputs.iter().any(|output| &output.name == name))
        {
            continue;
        }
        let live = state
            .layer_shell_state
            .layer_surfaces()
            .any(|surface| surface.wl_surface() == &record.surface);
        if !live {
            continue;
        }
        let entry = (
            record.surface.clone(),
            LayerRequest::of(&record.surface),
            record.layer,
        );
        match groups.iter_mut().find(|(k, _)| *k == record.output_name) {
            Some((_, list)) => list.push(entry),
            None => groups.push((record.output_name.clone(), vec![entry])),
        }
    }
    groups
}

/// Re-arrange every layer surface after a commit and send a configure
/// only where the arranged size changed. Surfaces arrange per output,
/// with exclusive zones and margins applied (see [`arrange`]).
pub fn arrange_after_commit(state: &State) {
    for (key, list) in output_requests(state) {
        let size = state.size_for_output(key.as_deref());
        let reqs: Vec<LayerRequest> = list.iter().map(|(_, r, _)| *r).collect();
        let rects = arrange(Rectangle::from_size(size), &reqs);
        for ((wl, _, _), rect) in list.iter().zip(rects) {
            let Some(surface) = state
                .layer_shell_state
                .layer_surfaces()
                .find(|s| s.wl_surface() == wl)
            else {
                continue;
            };
            if surface.current_state().size != Some(rect.size) {
                surface.with_pending_state(|pending| {
                    pending.size = Some(rect.size);
                });
                surface.send_pending_configure();
            }
        }
    }
}

/// Server-side record of one mapped layer surface (the slice-1 panel).
#[derive(Debug, Clone)]
pub struct PanelSurface {
    /// Namespace the client advertised (the shell panel uses
    /// `roost-shell-panel`).
    pub namespace: String,
    /// Layer the surface was created on (the panel uses top).
    pub layer: Layer,
    /// Whether the client has acknowledged any configure yet.
    pub configured: bool,
    /// Inventory name of the output the surface was created on
    /// (`None` for unbound/global surfaces, which arrange against
    /// primary). Resolved once at creation: the client's resource
    /// identifies the output through its protocol handle.
    pub output_name: Option<String>,
    pub(crate) surface: WlSurface,
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // A reused wl_surface gets a live layer role again.
        self.dead_layer_surfaces
            .remove(&smithay::reexports::wayland_server::Resource::id(
                surface.wl_surface(),
            ));
        let output_name = match output {
            Some(resource) => {
                let Some(bound) = Output::from_resource(&resource) else {
                    surface.send_close();
                    return;
                };
                // An old resource survives withdrawing its global. Connector
                // name reuse cannot authorize a new role on a replacement.
                if !self
                    .outputs
                    .iter()
                    .any(|entry| entry.output.as_ref() == Some(&bound))
                {
                    surface.send_close();
                    return;
                }
                Some(bound.name())
            }
            None => None,
        };
        // Bare initial configure (the protocol requires one before
        // the first commit); real geometry follows on commit via
        // `arrange_after_commit`, once the client's size/anchor
        // requests have arrived.
        surface.send_configure();
        self.panel_surfaces.push(PanelSurface {
            namespace,
            layer,
            configured: false,
            output_name,
            surface: surface.wl_surface().clone(),
        });
        self.refresh_initial_surface_scale(surface.wl_surface());
    }

    fn ack_configure(&mut self, surface: WlSurface, _configure: LayerSurfaceConfigure) {
        if let Some(record) = self
            .panel_surfaces
            .iter_mut()
            .find(|record| record.surface == surface)
        {
            record.configured = true;
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let gone = surface.wl_surface().clone();
        self.panel_surfaces.retain(|record| record.surface != gone);
        // Keep only the id (see `dead_layer_surfaces`): holding the
        // `WlSurface` would pin its last buffer and SHM pool mapping.
        // A fresh role on the same surface clears the entry in
        // `new_layer_surface`; stale ids never match a live surface
        // because object ids are never reused within a client.
        self.dead_layer_surfaces
            .insert(smithay::reexports::wayland_server::Resource::id(&gone));
    }
}

delegate_layer_shell!(State);

/// Where a layer surface belongs in the render stack.
fn layer_order(layer: Layer) -> u8 {
    match layer {
        Layer::Background => 0,
        Layer::Bottom => 1,
        Layer::Top => 2,
        Layer::Overlay => 3,
    }
}

/// Render placement for mapped layer surfaces: position from the
/// client anchor, size from the last configured server state.
/// Surfaces with no configured size yet are skipped. Sorted
/// background-to-overlay; callers draw windows first, then these.
pub fn layer_layout(state: &State) -> Vec<(WlSurface, (i32, i32), Layer)> {
    let mut placed = Vec::new();
    for (key, list) in output_requests(state) {
        let size = state.size_for_output(key.as_deref());
        let (ox, oy) = state.loc_for_output(key.as_deref());
        let reqs: Vec<LayerRequest> = list.iter().map(|(_, r, _)| *r).collect();
        let rects = arrange(Rectangle::from_size(size), &reqs);
        for ((wl, _, layer), rect) in list.iter().zip(rects) {
            // Skip surfaces never configured with a size yet.
            let configured = state
                .layer_shell_state
                .layer_surfaces()
                .find(|s| s.wl_surface() == wl)
                .and_then(|s| s.current_state().size)
                .is_some();
            if !configured {
                continue;
            }
            placed.push((wl.clone(), (ox + rect.loc.x, oy + rect.loc.y), *layer));
        }
    }
    placed.sort_by_key(|(_, _, layer)| layer_order(*layer));
    placed
}

/// The topmost mapped surface on the top or overlay layer that asked
/// for exclusive keyboard interactivity: under wlr-layer-shell it gets
/// the keyboard while mapped (a modal dialog, the unlock prompt).
pub fn exclusive_keyboard_layer(state: &State) -> Option<WlSurface> {
    layer_layout(state)
        .into_iter()
        .rev()
        .filter(|(_, _, layer)| matches!(layer, Layer::Top | Layer::Overlay))
        .map(|(surface, _, _)| surface)
        .find(|surface| {
            smithay::wayland::compositor::with_states(surface, |states| {
                states
                    .cached_state
                    .get::<LayerSurfaceCachedState>()
                    .current()
                    .keyboard_interactivity
                    == smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive
            })
        })
}

/// Exclusive popups use their own accelerator mode. The shell's overview also
/// owns the keyboard exclusively, but retains overview navigation/accelerators.
pub fn exclusive_popup_keyboard_layer(state: &State) -> Option<WlSurface> {
    let surface = exclusive_keyboard_layer(state)?;
    (state.overview_surface().as_ref() != Some(&surface)).then_some(surface)
}

/// Whether a press on a layer surface with this keyboard interactivity
/// gives it the keyboard. wlr-layer-shell: a surface asking for `none`
/// "is not interested in keyboard events and the compositor should
/// never assign it the keyboard focus". IBus's candidate window is one:
/// clicking a candidate must leave the keyboard (and with it the text
/// field's input method) where it was, as in GNOME Shell.
pub fn press_takes_keyboard(interactivity: KeyboardInteractivity) -> bool {
    interactivity != KeyboardInteractivity::None
}

/// [`press_takes_keyboard`] for a mapped layer surface.
pub fn surface_takes_keyboard_on_press(surface: &WlSurface) -> bool {
    compositor::with_states(surface, |states| {
        press_takes_keyboard(
            states
                .cached_state
                .get::<LayerSurfaceCachedState>()
                .current()
                .keyboard_interactivity,
        )
    })
}

/// Frame draw order from a bottom-to-top stack: Smithay 0.7's
/// `draw_render_elements` draws the first element topmost (it reverses
/// the slice internally for painter's order), while the runtime builds
/// windows bottom-to-top followed by background-to-overlay layers.
/// Passing that stack straight through inverts the session: the banner
/// draws beneath the dock and floating windows stack upside down.
/// Reverse once so the first element is the topmost surface.
pub fn front_to_back<T>(bottom_to_top: Vec<T>) -> Vec<T> {
    bottom_to_top.into_iter().rev().collect()
}

/// Topmost-first hit test over placed layer rects `(x, y, w, h)`.
/// Pure so unit tests pin the ordering without a live backend: later
/// rects paint over earlier ones, so the last containing rect wins.
pub fn hit_layer_rect(rects: &[(i32, i32, i32, i32)], x: i32, y: i32) -> Option<usize> {
    rects
        .iter()
        .rposition(|(rx, ry, w, h)| x >= *rx && x < rx + w && y >= *ry && y < ry + h)
}

/// Topmost layer surface at output coordinates, with its placed origin.
/// Returns `None` where no mapped layer surface covers the point, so the
/// caller falls through to window routing.
/// Whether `surface` accepts input at `local` (surface coordinates):
/// inside its committed input region, or anywhere when it set none.
pub fn accepts_input(surface: &WlSurface, local: (i32, i32)) -> bool {
    smithay::wayland::compositor::with_states(surface, |states| {
        let mut attrs = states
            .cached_state
            .get::<smithay::wayland::compositor::SurfaceAttributes>();
        attrs
            .current()
            .input_region
            .as_ref()
            .is_none_or(|region| region.contains(local))
    })
}

pub fn topmost_layer_at(state: &State, x: i32, y: i32) -> Option<(WlSurface, (i32, i32))> {
    let placed = layer_layout(state);
    let rects: Vec<(i32, i32, i32, i32)> = placed
        .iter()
        .map(|(surface, (ox, oy), _)| {
            let (w, h) = state
                .layer_shell_state
                .layer_surfaces()
                .find(|handle| handle.wl_surface() == surface)
                .and_then(|handle| handle.current_state().size)
                .map(|size| (size.w, size.h))
                .unwrap_or((0, 0));
            // A surface takes no input outside its wl_surface input
            // region (no region: all of it), so a click-through overlay
            // such as the overview's preview chrome passes presses on.
            if accepts_input(surface, (x - ox, y - oy)) {
                (*ox, *oy, w, h)
            } else {
                (*ox, *oy, 0, 0)
            }
        })
        .collect();
    hit_layer_rect(&rects, x, y).map(|index| {
        let (surface, origin, _) = &placed[index];
        (surface.clone(), *origin)
    })
}

impl State {
    /// Send real closed once and immediately revoke placement/input/overview
    /// membership. The client may stay alive indefinitely after receiving it;
    /// waiting for role destruction would remap it at a fallback output origin.
    pub fn retire_layer_output(&mut self, output: &str) {
        let surfaces: Vec<_> = self
            .panel_surfaces
            .iter()
            .filter(|record| record.output_name.as_deref() == Some(output))
            .map(|record| record.surface.clone())
            .collect();
        for surface in &surfaces {
            if let Some(layer) = self
                .layer_shell_state
                .layer_surfaces()
                .find(|layer| layer.wl_surface() == surface)
            {
                layer.send_close();
            }
        }
        self.panel_surfaces
            .retain(|record| record.output_name.as_deref() != Some(output));
    }

    /// Layer surfaces the server currently tracks (the slice-1 panel).
    pub fn panel_surfaces(&self) -> Vec<PanelSurface> {
        self.panel_surfaces.clone()
    }

    /// Live handles to all known layer surfaces, for configure/close
    /// driving by the runtime and tests.
    pub fn layer_surfaces(&self) -> Vec<LayerSurface> {
        self.layer_shell_state.layer_surfaces().collect()
    }

    /// The shell's overview layer surface, if currently mapped. The
    /// window manager parks keyboard (and selection) focus here while
    /// the overview is open.
    pub fn overview_surface(&self) -> Option<WlSurface> {
        self.panel_surfaces
            .iter()
            .find(|record| record.namespace == OVERVIEW_NAMESPACE)
            .map(|record| record.surface.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{front_to_back, hit_layer_rect, press_takes_keyboard, KeyboardInteractivity};

    #[test]
    fn a_press_never_gives_the_keyboard_to_a_surface_that_wants_none() {
        assert!(!press_takes_keyboard(KeyboardInteractivity::None));
        assert!(press_takes_keyboard(KeyboardInteractivity::OnDemand));
        assert!(press_takes_keyboard(KeyboardInteractivity::Exclusive));
    }

    #[test]
    fn topmost_rect_wins_and_edges_hold() {
        // Panel strip under a popup: the later (popup) rect wins overlap.
        let rects = [(0, 0, 1280, 32), (976, 40, 292, 200)];
        assert_eq!(hit_layer_rect(&rects, 1000, 100), Some(1));
        assert_eq!(hit_layer_rect(&rects, 100, 16), Some(0));
        assert_eq!(hit_layer_rect(&rects, 10, 500), None);
        // Right/bottom edges are exclusive.
        assert_eq!(hit_layer_rect(&rects, 1280, 16), None);
        assert_eq!(hit_layer_rect(&rects, 1279, 16), Some(0));
        // Zero-size rects never hit.
        assert_eq!(hit_layer_rect(&[(5, 5, 0, 0)], 5, 5), None);
    }

    #[test]
    fn draw_order_is_topmost_first() {
        // Bottom-to-top stack as the runtime builds it: windows in
        // stacking order, then background-to-overlay layers. The draw
        // slice must lead with the topmost surface (the Overlay banner
        // over the dock, the dock over the panel, the panel over the
        // focused window) or lower layers cover the banner.
        let stack = vec!["win-bottom", "win-focused", "panel", "dock", "banner"];
        assert_eq!(
            front_to_back(stack),
            vec!["banner", "dock", "panel", "win-focused", "win-bottom",]
        );
        // Empty and singleton stacks are fixed points.
        assert!(front_to_back(Vec::<u8>::new()).is_empty());
        assert_eq!(front_to_back(vec![7]), vec![7]);
    }
}

#[cfg(test)]
mod arrange_tests {
    use super::*;

    fn req(anchor: Anchor, w: i32, h: i32, zone: ExclusiveZone) -> LayerRequest {
        LayerRequest {
            anchor,
            size: (w, h).into(),
            margin: (0, 0, 0, 0),
            zone,
        }
    }

    fn output() -> Rectangle<i32, Logical> {
        Rectangle::from_size((1280, 800).into())
    }

    #[test]
    fn exclusive_panel_pushes_neutral_surfaces_below_it() {
        let panel = req(
            Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
            0,
            32,
            ExclusiveZone::Exclusive(32),
        );
        let mut search = req(Anchor::TOP, 370, 48, ExclusiveZone::Neutral);
        search.margin = (12, 0, 0, 0);
        let rects = arrange(output(), &[panel, search]);
        assert_eq!(rects[0], Rectangle::new((0, 0).into(), (1280, 32).into()));
        // Centered horizontally, 12 px below the panel.
        assert_eq!(rects[1], Rectangle::new((455, 44).into(), (370, 48).into()));
    }

    #[test]
    fn dont_care_surfaces_ignore_exclusive_zones() {
        let panel = req(
            Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
            0,
            32,
            ExclusiveZone::Exclusive(32),
        );
        let lock = req(Anchor::all(), 0, 0, ExclusiveZone::DontCare);
        let rects = arrange(output(), &[panel, lock]);
        assert_eq!(rects[1], output());
    }

    #[test]
    fn stretched_surfaces_respect_margins_and_both_zones() {
        let top = req(
            Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
            0,
            32,
            ExclusiveZone::Exclusive(32),
        );
        let bottom = req(
            Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            0,
            60,
            ExclusiveZone::Exclusive(60),
        );
        let mut grid = req(Anchor::all(), 0, 0, ExclusiveZone::Neutral);
        grid.margin = (64, 0, 112, 0);
        let rects = arrange(output(), &[top, bottom, grid]);
        assert_eq!(rects[1], Rectangle::new((0, 740).into(), (1280, 60).into()));
        assert_eq!(
            rects[2],
            Rectangle::new((0, 32 + 64).into(), (1280, 800 - 32 - 60 - 64 - 112).into())
        );
    }

    #[test]
    fn exclusive_zone_on_a_corner_anchor_is_neutral() {
        let corner = req(
            Anchor::TOP | Anchor::RIGHT,
            300,
            40,
            ExclusiveZone::Exclusive(40),
        );
        let other = req(Anchor::TOP, 100, 20, ExclusiveZone::Neutral);
        let rects = arrange(output(), &[corner, other]);
        assert_eq!(rects[0].loc, (980, 0).into());
        assert_eq!(rects[1].loc.y, 0, "a corner surface reserves nothing");
    }
}
