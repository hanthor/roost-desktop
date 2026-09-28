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
    wayland::shell::wlr_layer::{
        Layer, LayerSurface, LayerSurfaceConfigure, WlrLayerShellHandler, WlrLayerShellState,
    },
};

use crate::State;

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
        // Suggest geometry right away so the client can ack and commit;
        // the client carries its own size/anchor/zone requests.
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
}
