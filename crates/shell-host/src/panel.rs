//! Wayland client setup for the supervised shell host's Activities panel.
//!
//! The shell host is a separate Wayland client (001 spec R3/R5, ADR 0003):
//! it connects via `WAYLAND_DISPLAY`, binds `zwlr_layer_shell_v1`, and
//! creates one top-anchored layer surface carrying the Activities trigger
//! and window list. No UI logic runs inside the compositor process.
//!
//! # Deferred runtime
//!
//! Our test compositor does not implement layer-shell yet, so running this
//! binary end to end is **deferred**: against a compositor without
//! `zwlr_layer_shell_v1` the binary exits with a clear error instead of
//! guessing a fallback surface role (a fallback would silently misplace
//! the panel). The code below must compile and its structure — connect,
//! bind, create top-anchored surface, ack configures, run until closed —
//! must match the protocol. Live-compositor bring-up happens with the
//! compositor's layer-shell support, not here.
//!
//! # Supervision seam (ADR 0003)
//!
//! The compositor spawns this binary as a child process with a
//! nested-session `WAYLAND_DISPLAY` set for the child only. A crash is a
//! plain process exit: application Wayland connections are untouched, and
//! on restart the compositor resynchronizes this client from a full state
//! snapshot before ordered changes.

use std::fmt;

use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, BindError, GlobalListContents},
    protocol::{
        wl_compositor::WlCompositor, wl_output::WlOutput, wl_registry, wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{
        Anchor, Event as LayerSurfaceEvent, KeyboardInteractivity, ZwlrLayerSurfaceV1,
    },
};

use crate::model::ShellModel;

/// Namespace advertised for the panel layer surface.
pub const PANEL_NAMESPACE: &str = "rwd-shell-panel";
/// Fixed panel height in logical pixels; also the exclusive zone.
pub const PANEL_HEIGHT: u32 = 32;

/// Tunables for the panel surface. Defaults give a top-anchored,
/// full-width strip reserving an exclusive zone.
#[derive(Debug, Clone)]
pub struct PanelConfig {
    /// Layer-shell namespace for the panel surface.
    pub namespace: String,
    /// Panel height in logical pixels.
    pub height: u32,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            namespace: PANEL_NAMESPACE.to_owned(),
            height: PANEL_HEIGHT,
        }
    }
}

/// Ways panel startup can fail before the event loop runs.
#[derive(Debug)]
pub enum PanelError {
    /// No compositor reachable via `WAYLAND_DISPLAY`.
    Connect(wayland_client::ConnectError),
    /// Registry snapshot failed.
    Registry(wayland_client::globals::GlobalError),
    /// Compositor does not offer `wl_compositor`.
    NoCompositor(BindError),
    /// Compositor does not offer `zwlr_layer_shell_v1` (our test
    /// compositor today: live runtime deferred, see module docs).
    NoLayerShell(BindError),
    /// Event-loop dispatch failed (e.g. compositor went away).
    Dispatch(wayland_client::DispatchError),
}

impl fmt::Display for PanelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "cannot connect via WAYLAND_DISPLAY: {err}"),
            Self::Registry(err) => write!(f, "registry snapshot failed: {err}"),
            Self::NoCompositor(err) => write!(f, "compositor offers no wl_compositor: {err}"),
            Self::NoLayerShell(err) => write!(
                f,
                "compositor offers no zwlr_layer_shell_v1 (layer-shell runtime deferred): {err}"
            ),
            Self::Dispatch(err) => write!(f, "event loop dispatch failed: {err}"),
        }
    }
}

impl std::error::Error for PanelError {}

/// Application state driven by the Wayland event queue.
pub struct ShellHost {
    /// Shell-side view state (window list, overview flag).
    pub model: ShellModel,
    panel: PanelConfig,
    running: bool,
    surface: Option<WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
}

impl ShellHost {
    /// Create the panel `wl_surface` plus its top-anchored layer surface.
    ///
    /// `output` is `None` so the compositor places the panel on the
    /// default output; anchoring to top/left/right plus the exclusive
    /// zone keeps application windows clear of the strip.
    fn create_panel_surface(
        &mut self,
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        qh: &QueueHandle<Self>,
    ) {
        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Top,
            self.panel.namespace.clone(),
            qh,
            (),
        );
        layer_surface.set_size(0, self.panel.height);
        layer_surface.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
        layer_surface.set_exclusive_zone(self.panel.height as i32);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        surface.commit();
        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);
    }

    /// Whether the event loop should keep dispatching.
    ///
    /// Exiting the loop is the supervised-crash seam: the compositor
    /// (ADR 0003) observes the disconnect, shows its recovery affordance,
    /// and restarts this binary with bounded backoff.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Acknowledge a configure and re-commit so the panel takes effect.
    fn ack_configure(&mut self, layer_surface: &ZwlrLayerSurfaceV1, serial: u32) {
        layer_surface.ack_configure(serial);
        if let Some(surface) = &self.surface {
            surface.commit();
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ShellHost {
    fn event(
        _state: &mut Self,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Globals are bound once at startup; dynamic add/remove needs no
        // reaction from the slice-1 panel.
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for ShellHost {
    fn event(
        state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: LayerSurfaceEvent,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            LayerSurfaceEvent::Configure { serial, .. } => {
                state.ack_configure(proxy, serial);
            }
            LayerSurfaceEvent::Closed => {
                state.running = false;
            }
            _ => {}
        }
    }
}

// The remaining bound globals carry no events the panel handles.
delegate_noop!(ShellHost: ignore WlCompositor);
delegate_noop!(ShellHost: ignore WlSurface);
delegate_noop!(ShellHost: ignore WlOutput);
delegate_noop!(ShellHost: ignore ZwlrLayerShellV1);

/// Connect plus event-loop setup kept together for `main`: builds the
/// connection, queue, and host, then dispatches until close.
pub fn run_panel(panel: PanelConfig) -> Result<(), PanelError> {
    let conn = Connection::connect_to_env().map_err(PanelError::Connect)?;
    let (globals, mut queue) =
        registry_queue_init::<ShellHost>(&conn).map_err(PanelError::Registry)?;
    let qh = queue.handle();

    let compositor: WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .map_err(PanelError::NoCompositor)?;
    let layer_shell: ZwlrLayerShellV1 = globals
        .bind(&qh, 3..=5, ())
        .map_err(PanelError::NoLayerShell)?;

    let mut host = ShellHost {
        model: ShellModel::new(),
        panel,
        running: true,
        surface: None,
        layer_surface: None,
    };
    host.create_panel_surface(&compositor, &layer_shell, &qh);
    drop((globals, compositor, layer_shell));

    queue.roundtrip(&mut host).map_err(PanelError::Dispatch)?;
    while host.is_running() {
        queue
            .blocking_dispatch(&mut host)
            .map_err(PanelError::Dispatch)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_defaults_are_top_strip_with_namespace() {
        let config = PanelConfig::default();
        assert_eq!(config.namespace, PANEL_NAMESPACE);
        assert_eq!(config.height, PANEL_HEIGHT);
        assert!(config.height > 0, "exclusive zone must reserve space");
    }
}
