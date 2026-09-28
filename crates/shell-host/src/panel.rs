//! Wayland client setup for the supervised shell host's Activities panel.
//!
//! The shell host is a separate Wayland client (001 spec R3/R5, ADR 0003):
//! it connects via `WAYLAND_DISPLAY`, binds `zwlr_layer_shell_v1`, and
//! creates one top-anchored layer surface carrying the Activities trigger
//! and window list. No UI logic runs inside the compositor process.
//!
//! # Live runtime
//!
//! The compositor serves `zwlr_layer_shell_v1` (see `rwd_compositor::layer`),
//! so this binary attaches for real: connect, bind, create the top-anchored
//! surface, ack configures, run until closed. Against a compositor without
//! the global it exits with a clear error instead of guessing a fallback
//! surface role (a fallback would silently misplace the panel).
//!
//! # Supervision seam (ADR 0003)
//!
//! The compositor spawns this binary as a child process with a
//! nested-session `WAYLAND_DISPLAY` set for the child only. A crash is a
//! plain process exit: application Wayland connections are untouched, and
//! on restart the compositor resynchronizes this client from a full state
//! snapshot before ordered changes.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

use crate::control::{ControlClient, ControlError, Handled};
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
    /// Compositor does not offer `zwlr_layer_shell_v1` (no fallback
    /// surface role: misplacing the panel silently would be worse).
    NoLayerShell(BindError),
    /// Event-loop dispatch failed (e.g. compositor went away).
    Dispatch(wayland_client::DispatchError),
    /// Control channel failed (connect, handshake, or snapshot).
    Control(ControlError),
    /// Event-queue flush failed (message only; no content).
    Flush(String),
}

impl fmt::Display for PanelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(err) => write!(f, "cannot connect via WAYLAND_DISPLAY: {err}"),
            Self::Registry(err) => write!(f, "registry snapshot failed: {err}"),
            Self::NoCompositor(err) => write!(f, "compositor offers no wl_compositor: {err}"),
            Self::NoLayerShell(err) => {
                write!(f, "compositor offers no zwlr_layer_shell_v1: {err}")
            }
            Self::Dispatch(err) => write!(f, "event loop dispatch failed: {err}"),
            Self::Control(err) => write!(f, "control channel failed: {err}"),
            Self::Flush(err) => write!(f, "event queue flush failed: {err}"),
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

    /// Replace the overview state with the control client's live model:
    /// current window list, workspaces, selection, and the compositor's
    /// overview-open intent (002 R1). The panel renders only this
    /// compositor truth.
    fn sync_overview(&mut self, client: &ControlClient) {
        let model = client.model();
        self.model
            .apply_window_list(model.windows().to_vec(), model.workspaces().to_vec());
        if let Some(selected) = model.selected() {
            let _ = self.model.select_window(selected);
        }
        self.model.set_overview_open(model.is_overview_open());
    }
}

/// Whether a control error is just "nothing to read yet".
fn is_would_block(err: &ControlError) -> bool {
    matches!(err, ControlError::Io(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

/// Connect the control channel and complete the handshake plus the
/// initial snapshot, with bounded retries (startup only; the steady
/// loop never blocks). Returns a client holding live overview state.
fn attach_control(path: &Path) -> Result<ControlClient, PanelError> {
    let mut control = ControlClient::connect(path)
        .map_err(ControlError::Io)
        .map_err(PanelError::Control)?;
    control.send_hello().map_err(PanelError::Control)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match control.await_hello() {
            Ok(()) => break,
            Err(e) if is_would_block(&e) => {
                if Instant::now() >= deadline {
                    return Err(PanelError::Control(ControlError::Unexpected(
                        "control hello timeout".to_owned(),
                    )));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(PanelError::Control(e)),
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match control.poll() {
            Ok(Handled::Snapshot { .. }) => break,
            Ok(_) => {}
            Err(e) if is_would_block(&e) => {
                if Instant::now() >= deadline {
                    return Err(PanelError::Control(ControlError::Unexpected(
                        "control snapshot timeout".to_owned(),
                    )));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => return Err(PanelError::Control(e)),
        }
    }
    Ok(control)
}

/// One nonblocking control step inside the panel loop: apply whatever
/// frame is waiting into the overview model, re-request the snapshot
/// after a revision gap, and log (never crash on) typed errors — the
/// connection stays usable.
fn drive_control(control: &mut ControlClient, host: &mut ShellHost) {
    match control.poll() {
        Err(e) if is_would_block(&e) => {}
        Ok(Handled::Gap { .. }) => {
            host.sync_overview(control);
            if let Err(e) = control.request_snapshot() {
                eprintln!("rwd-shell-host: control resnapshot failed: {e}");
            }
        }
        Ok(_) => host.sync_overview(control),
        Err(e) => eprintln!("rwd-shell-host: control error: {e}"),
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
    run_panel_with_control(panel, None)
}

/// [`run_panel`] plus the live overview feed: when `control_path` is
/// set, the panel also speaks the control channel — handshake and
/// initial snapshot up front, then one nonblocking control step per
/// panel-loop iteration keeps the overview model on compositor truth.
/// The compositor sets `RWD_CONTROL_SOCKET` for the supervised child;
/// running without it leaves a panel with an empty overview.
pub fn run_panel_with_control(
    panel: PanelConfig,
    control_path: Option<PathBuf>,
) -> Result<(), PanelError> {
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

    let mut control = match control_path {
        Some(path) => {
            let client = attach_control(&path)?;
            host.sync_overview(&client);
            Some(client)
        }
        None => None,
    };

    queue.roundtrip(&mut host).map_err(PanelError::Dispatch)?;
    if let Some(control) = control.as_mut() {
        // Provisional pacing: poll both sides at 200 Hz instead of
        // blocking on Wayland events, so control frames land while the
        // user is idle. Pure panel runs keep the blocking loop below.
        while host.is_running() {
            queue
                .dispatch_pending(&mut host)
                .map_err(PanelError::Dispatch)?;
            drive_control(control, &mut host);
            queue
                .flush()
                .map_err(|e| PanelError::Flush(e.to_string()))?;
            std::thread::sleep(Duration::from_millis(5));
        }
    } else {
        while host.is_running() {
            queue
                .blocking_dispatch(&mut host)
                .map_err(PanelError::Dispatch)?;
        }
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

    /// Live attach of the real [`ShellHost`] against the compositor's
    /// layer-shell server: the panel surface appears server-side with our
    /// namespace, the configure round-trip acks, and server close stops
    /// the loop. Headless and deterministic: a socketpair client plus a
    /// bounded pump budget, no sleeps.
    mod live {
        use std::os::unix::net::UnixStream;

        use rwd_compositor::{
            control::ControlHub,
            state::{StateModel, TokenStore},
            TestCompositor, SEAT_NAME,
        };
        use wayland_client::{
            protocol::{wl_compositor::WlCompositor, wl_registry},
            Connection, Dispatch, EventQueue, QueueHandle,
        };
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

        use crate::control::{ControlClient, Handled};
        use crate::model::ShellModel;

        use super::super::{is_would_block, PanelConfig, ShellHost, PANEL_NAMESPACE};

        const PUMP_ROUNDS: usize = 200;

        /// Registry observer on a throwaway queue, used only to learn
        /// global names before binding on the panel queue.
        #[derive(Default)]
        struct Collector {
            compositor: Option<(u32, u32)>,
            layer_shell: Option<(u32, u32)>,
        }

        impl Dispatch<wl_registry::WlRegistry, ()> for Collector {
            fn event(
                state: &mut Self,
                _: &wl_registry::WlRegistry,
                event: <wl_registry::WlRegistry as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } = event
                {
                    match interface.as_str() {
                        "wl_compositor" => state.compositor = Some((name, version)),
                        "zwlr_layer_shell_v1" => state.layer_shell = Some((name, version)),
                        _ => {}
                    }
                }
            }
        }

        fn pump_server(
            comp: &mut TestCompositor,
            queue: &mut EventQueue<ShellHost>,
            host: &mut ShellHost,
        ) {
            queue.flush().unwrap();
            comp.pump();
            if let Some(guard) = queue.prepare_read() {
                guard.read().unwrap();
            }
            queue.dispatch_pending(host).unwrap();
        }

        #[test]
        fn panel_attaches_acknowledges_and_stops_on_close() {
            let mut comp = TestCompositor::new();
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();

            // Learn global names on a throwaway queue first, then bind
            // them onto the panel queue for the real host.
            let mut queue = conn.new_event_queue();
            let qh = queue.handle();
            let mut aux = conn.new_event_queue();
            let aux_qh = aux.handle();
            let mut collector = Collector::default();
            let aux_registry = conn.display().get_registry(&aux_qh, ());
            for _ in 0..PUMP_ROUNDS {
                aux.flush().unwrap();
                comp.pump();
                if let Some(guard) = aux.prepare_read() {
                    guard.read().unwrap();
                }
                aux.dispatch_pending(&mut collector).unwrap();
                if collector.compositor.is_some() && collector.layer_shell.is_some() {
                    break;
                }
            }
            let (compositor_name, compositor_version) =
                collector.compositor.expect("wl_compositor advertised");
            let (layer_name, layer_version) = collector
                .layer_shell
                .expect("zwlr_layer_shell_v1 advertised");
            let compositor: WlCompositor = aux_registry.bind::<WlCompositor, _, _>(
                compositor_name,
                compositor_version.min(6),
                &qh,
                (),
            );
            let layer_shell: ZwlrLayerShellV1 = aux_registry.bind::<ZwlrLayerShellV1, _, _>(
                layer_name,
                layer_version.min(5),
                &qh,
                (),
            );
            drop((aux, aux_registry, collector));

            let mut host = ShellHost {
                model: ShellModel::new(),
                panel: PanelConfig::default(),
                running: true,
                surface: None,
                layer_surface: None,
            };
            host.create_panel_surface(&compositor, &layer_shell, &qh);
            drop((compositor, layer_shell));

            // The real panel surface arrives server-side with our
            // namespace, and the configure round-trip acks through the
            // real dispatch path.
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                if comp
                    .state
                    .panel_surfaces()
                    .first()
                    .is_some_and(|panel| panel.configured)
                {
                    break;
                }
            }
            let panels = comp.state.panel_surfaces();
            assert_eq!(panels.len(), 1, "one panel surface tracked");
            assert_eq!(panels[0].namespace, PANEL_NAMESPACE);
            assert!(panels[0].configured, "panel acked the configure");
            assert!(host.is_running(), "panel keeps running");

            // Server close stops the loop: the supervised-crash seam.
            comp.state.layer_surfaces()[0].send_close();
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                if !host.is_running() {
                    break;
                }
            }
            assert!(!host.is_running(), "close stops the panel loop");
        }

        /// The binary's overview feed: a control client handshaked
        /// against a live hub fills the host model from the snapshot,
        /// then follows a later delta — the same `sync_overview` path
        /// `run_panel_with_control` drives per loop.
        #[test]
        fn control_feeds_overview_model() {
            use std::rc::Rc;

            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("control.sock");
            let mut model = StateModel::new();
            model.insert("alpha", Some("com.example.alpha"), 0);
            model.insert("beta", Some("com.example.beta"), 0);
            let mut hub =
                ControlHub::bind(socket_path.clone(), Rc::new(TokenStore::new()), SEAT_NAME)
                    .unwrap();

            let mut client = ControlClient::connect(&socket_path).unwrap();
            client.send_hello().unwrap();
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.await_hello() {
                    Ok(()) => break,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("hello failed: {e}"),
                }
            }
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Snapshot { .. }) => break,
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("snapshot poll failed: {e}"),
                }
            }

            let mut host = ShellHost {
                model: ShellModel::new(),
                panel: PanelConfig::default(),
                running: true,
                surface: None,
                layer_surface: None,
            };
            host.sync_overview(&client);
            assert_eq!(host.model.windows().len(), 2);
            let titles: Vec<&str> = host
                .model
                .windows()
                .iter()
                .map(|w| w.title.as_str())
                .collect();
            assert!(titles.contains(&"alpha"));
            assert!(titles.contains(&"beta"));

            // A later model change flows through hub deltas into the
            // overview on the next sync.
            model.insert("gamma", Some("com.example.gamma"), 0);
            let want = model.revision();
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(_) => {
                        if client.revision() == Some(want) {
                            break;
                        }
                    }
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("delta poll failed: {e}"),
                }
            }
            assert_eq!(client.revision(), Some(want));
            host.sync_overview(&client);
            assert_eq!(host.model.windows().len(), 3);

            // Compositor overview intent reaches the host model on sync.
            assert!(!host.model.is_overview_open());
            hub.set_overview(true);
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Overview { open }) => {
                        assert!(open);
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("overview poll failed: {e}"),
                }
            }
            host.sync_overview(&client);
            assert!(host.model.is_overview_open());
            assert_eq!(host.model.windows().len(), 3);
        }
    }
}
