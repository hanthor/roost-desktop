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
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::{Logical, Size},
    wayland::{
        compositor,
        shell::wlr_layer::{
            Anchor, Layer, LayerSurface, LayerSurfaceCachedState, LayerSurfaceConfigure,
            WlrLayerShellHandler, WlrLayerShellState,
        },
    },
};

use crate::State;

/// Namespace the shell advertises for its overview layer surface
/// (mirrors `rwd_shell_host::OVERVIEW_NAMESPACE`; the crates cannot
/// share the const without a dependency cycle).
pub const OVERVIEW_NAMESPACE: &str = "rwd-shell-overview";

/// Arrange one layer surface against the output: axes anchored on
/// both edges take the output size, other axes take the client's
/// requested size.
///
/// Pure sizing — callers decide when to send. Sizing must run on
/// commit, not in `new_layer_surface`: the client's size/anchor
/// requests only arrive after the surface exists, so arranging at
/// creation always sees the default (empty) client state.
pub fn arranged_size(surface: &LayerSurface, output: Size<i32, Logical>) -> Size<i32, Logical> {
    let (requested, anchor) = compositor::with_states(surface.wl_surface(), |states| {
        let mut cached = states.cached_state.get::<LayerSurfaceCachedState>();
        let current = cached.current();
        (current.size, current.anchor)
    });
    let width = if anchor.contains(Anchor::LEFT) && anchor.contains(Anchor::RIGHT) {
        output.w
    } else {
        requested.w
    };
    let height = if anchor.contains(Anchor::TOP) && anchor.contains(Anchor::BOTTOM) {
        output.h
    } else {
        requested.h
    };
    Size::from((width.max(0), height.max(0)))
}

/// Re-arrange every layer surface after a commit and send a configure
/// only where the arranged size changed. The runtime calls this from
/// its commit handler: client size/anchor requests land with the
/// commit, so this is the earliest point the real geometry is known.
pub fn arrange_after_commit(state: &State) {
    for surface in state.layer_shell_state.layer_surfaces() {
        let size = arranged_size(&surface, state.output_size);
        if surface.current_state().size != Some(size) {
            surface.with_pending_state(|pending| {
                pending.size = Some(size);
            });
            surface.send_pending_configure();
        }
    }
}

/// Server-side record of one mapped layer surface (the slice-1 panel).
#[derive(Debug, Clone)]
pub struct PanelSurface {
    /// Namespace the client advertised (the shell panel uses
    /// `rwd-shell-panel`).
    pub namespace: String,
    /// Layer the surface was created on (the panel uses top).
    pub layer: Layer,
    /// Whether the client has acknowledged any configure yet.
    pub configured: bool,
    surface: WlSurface,
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // Bare initial configure (the protocol requires one before
        // the first commit); real geometry follows on commit via
        // `arrange_after_commit`, once the client's size/anchor
        // requests have arrived.
        surface.send_configure();
        self.panel_surfaces.push(PanelSurface {
            namespace,
            layer,
            configured: false,
            surface: surface.wl_surface().clone(),
        });
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
    let output = state.output_size;
    let mut placed = Vec::new();
    for record in &state.panel_surfaces {
        let Some(handle) = state
            .layer_shell_state
            .layer_surfaces()
            .find(|surface| surface.wl_surface() == &record.surface)
        else {
            continue;
        };
        let Some(size) = handle.current_state().size else {
            continue;
        };
        let anchor = compositor::with_states(handle.wl_surface(), |states| {
            states
                .cached_state
                .get::<LayerSurfaceCachedState>()
                .current()
                .anchor
        });
        let x = if anchor.contains(Anchor::LEFT) {
            0
        } else if anchor.contains(Anchor::RIGHT) {
            output.w - size.w
        } else {
            (output.w - size.w) / 2
        };
        let y = if anchor.contains(Anchor::TOP) {
            0
        } else if anchor.contains(Anchor::BOTTOM) {
            output.h - size.h
        } else {
            (output.h - size.h) / 2
        };
        placed.push((record.surface.clone(), (x, y), record.layer));
    }
    placed.sort_by_key(|(_, _, layer)| layer_order(*layer));
    placed
}

impl State {
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
