//! Nested session runtime (001 T1, ADR 0001).
//!
//! Owns the Smithay winit/calloop session: backend creation, client
//! acceptance over a private socket, output setup, per-tick dispatch, and
//! frame production. Window management, input routing, shell supervision,
//! and the control channel attach in later tasks; this module only drives
//! the loop and keeps the host environment untouched.
//!
//! The loop shape follows upstream `examples/minimal.rs` at the pinned
//! revision (manual repaint each round, toplevels stacked at the origin);
//! damage tracking arrives with a later task. Layer-shell is served from
//! [`State`](crate::State) (see [`crate::layer`]) so the supervised panel
//! attaches over this same display.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use calloop::EventLoop;
use smithay::{
    backend::{
        renderer::{
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            gles::GlesRenderer,
            utils::draw_render_elements,
            Color32F, Frame, Renderer,
        },
        winit::{self, WinitEvent},
    },
    desktop::utils::send_frames_surface_tree,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::Display,
    utils::{Rectangle, Transform},
    wayland::socket::ListeningSocketSource,
};

use crate::control::ControlHub;
use crate::state::TokenStore;
use crate::windows::{translate_input, WindowManager};

use crate::{ClientState, State};

/// Maximum loop rate: dispatch blocks up to this long waiting for events,
/// so the loop never spins, and repaint stays near 60 Hz. Damage-tracked
/// repaint replaces this provisional pacing in a later slice.
const FRAME_BUDGET: Duration = Duration::from_millis(16);

/// Default nested output size (physical pixels).
const DEFAULT_WIDTH: i32 = 1280;
const DEFAULT_HEIGHT: i32 = 800;

/// Nested session configuration: socket identity plus output geometry.
#[derive(Debug, Clone)]
pub struct NestedSession {
    /// Private Wayland socket name, e.g. `rwd-nested-<pid>`.
    pub socket_name: String,
    /// Output width in physical pixels.
    pub width: i32,
    /// Output height in physical pixels.
    pub height: i32,
}

impl NestedSession {
    /// Session with an explicit socket name and size.
    pub fn new(socket_name: String, width: i32, height: i32) -> Self {
        Self {
            socket_name,
            width,
            height,
        }
    }

    /// Default session: unique socket name from our pid, default size.
    pub fn default_for_pid() -> Self {
        Self::new(
            format!("rwd-nested-{}", std::process::id()),
            DEFAULT_WIDTH,
            DEFAULT_HEIGHT,
        )
    }
}

/// Summary of one nested run, for diagnostics (no sensitive content).
#[derive(Debug, Clone, Copy, Default)]
pub struct RunStats {
    /// Frames produced.
    pub frames: u64,
    /// Wayland clients accepted.
    pub clients: u64,
}

/// Ways the nested runtime can fail to start or run.
#[derive(Debug)]
pub enum RuntimeError {
    /// GLES/window backend creation failed (no host display or EGL).
    Backend(String),
    /// Control socket binding failed.
    Socket(String),
    /// calloop event-loop setup or dispatch failed.
    Loop(String),
    /// Client dispatch or frame production failed.
    Dispatch(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(e) => write!(f, "nested backend unavailable: {e}"),
            Self::Socket(e) => write!(f, "nested socket failed: {e}"),
            Self::Loop(e) => write!(f, "event loop failed: {e}"),
            Self::Dispatch(e) => write!(f, "client dispatch failed: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Apply nested-session environment hygiene (ADR 0003): point
/// `WAYLAND_DISPLAY` at our private socket for this process and its
/// children. The parent host environment is never touched — this runs in
/// our own process — and the previous value is returned so shutdown can
/// restore it.
pub fn apply_nested_env(socket_name: &str) -> Option<OsString> {
    let prev = std::env::var_os("WAYLAND_DISPLAY");
    std::env::set_var("WAYLAND_DISPLAY", socket_name);
    prev
}

/// Undo [`apply_nested_env`].
pub fn restore_env(prev: Option<OsString>) {
    match prev {
        Some(value) => std::env::set_var("WAYLAND_DISPLAY", value),
        None => std::env::remove_var("WAYLAND_DISPLAY"),
    }
}

/// Live nested session: display, protocol state, backend, and output.
pub struct Runtime {
    display: Display<State>,
    state: State,
    backend: winit::WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    manager: WindowManager,
    control: ControlHub,
    exit: bool,
    stats: RunStats,
}

/// Control socket path for a session: alongside the Wayland socket in the
/// runtime dir, so one session owns both (`rwd-<name>.control`).
pub fn control_socket_path(socket_name: &str) -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("rwd-{socket_name}.control"))
}

impl Runtime {
    /// Build the session: display plus globals, output, backend window,
    /// and private socket. Does not run the loop (see [`run`]).
    pub fn launch(
        session: &NestedSession,
    ) -> Result<(Self, EventLoop<'static, Self>), RuntimeError> {
        let display: Display<State> =
            Display::new().map_err(|e| RuntimeError::Loop(e.to_string()))?;
        let dh = display.handle();
        let mut state = State::new(&dh);
        let manager = WindowManager::new(&mut state);
        let tokens = std::rc::Rc::new(TokenStore::new());
        let control_path = control_socket_path(&session.socket_name);
        let control = ControlHub::bind(control_path, tokens, crate::SEAT_NAME)
            .map_err(|e| RuntimeError::Socket(e.to_string()))?;

        let output = Output::new(
            "rwd-0".to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "RWD".to_owned(),
                model: "Nested".to_owned(),
            },
        );
        let mode = Mode {
            size: (session.width, session.height).into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            None,
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        output.create_global::<State>(&dh);

        let (backend, winit_loop) =
            winit::init::<GlesRenderer>().map_err(|e| RuntimeError::Backend(e.to_string()))?;
        let socket = ListeningSocketSource::with_name(&session.socket_name)
            .map_err(|e| RuntimeError::Socket(e.to_string()))?;

        let event_loop = EventLoop::try_new().map_err(|e| RuntimeError::Loop(e.to_string()))?;
        event_loop
            .handle()
            .insert_source(winit_loop, |event, _, runtime: &mut Runtime| {
                runtime.on_winit_event(event);
            })
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;
        event_loop
            .handle()
            .insert_source(socket, |stream, _, runtime: &mut Runtime| {
                if let Ok(client) = runtime
                    .display
                    .handle()
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    let _ = client;
                    runtime.stats.clients += 1;
                }
            })
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;

        let runtime = Runtime {
            display,
            state,
            backend,
            output,
            manager,
            control,
            exit: false,
            stats: RunStats::default(),
        };
        Ok((runtime, event_loop))
    }

    /// Handle one backend event. Input routing arrives in T2; here only
    /// resize, redraw, and close affect the loop.
    fn on_winit_event(&mut self, event: WinitEvent) {
        match event {
            WinitEvent::Resized { size, .. } => {
                let mode = Mode {
                    size,
                    refresh: 60_000,
                };
                self.output
                    .change_current_state(Some(mode), None, None, None);
            }
            WinitEvent::CloseRequested => self.exit = true,
            WinitEvent::Input(event) => {
                if let Some(input) = translate_input(event) {
                    self.manager.on_input(&mut self.state, input);
                }
            }
            WinitEvent::Redraw | WinitEvent::Focus(_) => {}
        }
    }

    /// Dispatch clients, reconcile windows, produce one frame, and
    /// report whether to continue.
    fn tick(&mut self) -> Result<bool, RuntimeError> {
        self.display
            .dispatch_clients(&mut self.state)
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.display
            .flush_clients()
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.manager.reconcile(&mut self.state);
        let activated = self.control.poll(self.manager.model_mut());
        for id in activated {
            self.manager.focus(&mut self.state, Some(id));
        }
        self.render()?;
        Ok(!self.exit)
    }

    /// Render all mapped toplevels stacked at the origin, then send frame
    /// callbacks. T2 replaces the stacking with real window management.
    fn render(&mut self) -> Result<(), RuntimeError> {
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = self
                .backend
                .bind()
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = self
                .manager
                .visible_windows()
                .iter()
                .flat_map(|(surface, geometry)| {
                    render_elements_from_surface_tree(
                        renderer,
                        surface.wl_surface(),
                        (geometry.loc.x, geometry.loc.y),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect();
            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Normal)
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            frame
                .clear(Color32F::new(0.08, 0.09, 0.11, 1.0), &[damage])
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            let _ = frame
                .finish()
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        }
        // Provisional frame pacing: always send callbacks (zero throttle)
        // with this output as scan-out. Damage-tracked pacing is deferred.
        let output = self.output.clone();
        let time = Duration::from_millis(self.stats.frames.saturating_mul(16));
        for surface in self.state.toplevels() {
            send_frames_surface_tree(
                surface.wl_surface(),
                &output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
        self.backend
            .submit(Some(&[damage]))
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.stats.frames += 1;
        Ok(())
    }
}

/// Run a nested session to shutdown: drive the loop and report run
/// statistics.
///
/// Environment hygiene ([`apply_nested_env`]) is applied only after the
/// backend exists: the winit backend prefers Wayland when
/// `WAYLAND_DISPLAY` is set, so setting it first would point winit at our
/// own not-yet-existing socket.
pub fn run(session: &NestedSession) -> Result<RunStats, RuntimeError> {
    let (mut runtime, mut event_loop) = Runtime::launch(session)?;
    let prev_env = apply_nested_env(&session.socket_name);
    loop {
        let dispatched = event_loop
            .dispatch(Some(FRAME_BUDGET), &mut runtime)
            .map_err(|e| RuntimeError::Loop(e.to_string()));
        let ticked = dispatched.and_then(|_| runtime.tick());
        match ticked {
            Ok(true) => continue,
            done => {
                restore_env(prev_env);
                return done.map(|_| runtime.stats);
            }
        }
    }
}
