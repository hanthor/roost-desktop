//! Nested session runtime (001 T1, ADR 0001).
//!
//! Owns the Smithay winit/calloop session: backend creation, client
//! acceptance over a private socket, output setup, per-tick dispatch,
//! frame production, window management, input routing, shell supervision
//! (T5), and the control channel. The loop keeps the host environment
//! untouched.
//!
//! The loop shape follows upstream `examples/minimal.rs` at the pinned
//! revision (manual repaint each round, toplevels stacked at the origin);
//! damage tracking arrives with a later task. Layer-shell is served from
//! [`State`](crate::State) (see [`crate::layer`]) so the supervised panel
//! attaches over this same display.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use crate::lock::{content_visible, SessionLock, DEFAULT_IDLE_TIMEOUT_MS};
use crate::overlay::{overlay_key_for_keycode, Overlay, OverlayWindow};
use crate::state::TokenStore;
use crate::supervise::{RecoveryAction, RestartPolicy, ShellDriver, ShellStatus};
use crate::wallpaper::Wallpaper;
use crate::windows::{
    translate_input, ManagerInput, TriggerAction, TriggerState, WindowManager, ESCAPE_KEYCODE,
};
use roost_greeter::client::GreeterClient;

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
    /// Private Wayland socket name, e.g. `roost-nested-<pid>`.
    pub socket_name: String,
    /// Output width in physical pixels.
    pub width: i32,
    /// Output height in physical pixels.
    pub height: i32,
    /// Shell binary to supervise. `None` selects
    /// [`resolve_shell_bin`]: `ROOST_SHELL_BIN`, then the
    /// `roost-shell-host` sibling of this binary, then `PATH`.
    pub shell_bin: Option<std::path::PathBuf>,
}

impl NestedSession {
    /// Session with an explicit socket name and size.
    pub fn new(socket_name: String, width: i32, height: i32) -> Self {
        Self {
            socket_name,
            width,
            height,
            shell_bin: None,
        }
    }

    /// Default session: unique socket name from our pid, default size.
    pub fn default_for_pid() -> Self {
        Self::new(
            format!("roost-nested-{}", std::process::id()),
            DEFAULT_WIDTH,
            DEFAULT_HEIGHT,
        )
    }
}

/// Sibling binary next to `dir` when it exists as a file.
/// Shared by the session launcher and the shell resolver so both agree
/// on what "installed side by side" means.
pub fn sibling_binary(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let candidate = dir.join(name);
    candidate.is_file().then_some(candidate)
}

/// Directory holding this process's binary, for sibling resolution.
pub fn current_exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_owned()))
}

/// Shell binary for a session: explicit config, then `ROOST_SHELL_BIN`,
/// then the `roost-shell-host` sibling of this binary when it exists,
/// else a `PATH` lookup at spawn time.
pub fn resolve_shell_bin(configured: Option<&std::path::Path>) -> std::path::PathBuf {
    if let Some(path) = configured {
        return path.to_owned();
    }
    if let Ok(path) = std::env::var("ROOST_SHELL_BIN") {
        if !path.is_empty() {
            return std::path::PathBuf::from(path);
        }
    }
    if let Some(dir) = current_exe_dir() {
        if let Some(sibling) = sibling_binary(&dir, "roost-shell-host") {
            return sibling;
        }
    }
    std::path::PathBuf::from("roost-shell-host")
}

/// Summary of one nested run, for diagnostics (no sensitive content).
#[derive(Debug, Clone, Copy, Default)]
pub struct RunStats {
    /// Frames produced.
    pub frames: u64,
    /// Wayland clients accepted.
    pub clients: u64,
    /// Shell restarts consumed (initial spawn excluded).
    pub shell_restarts: u32,
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
    shell: ShellDriver,
    overlay: Overlay,
    wallpaper: Wallpaper,
    triggers: TriggerState,
    /// Compositor-owned session lock: flag plus idle accumulator fed
    /// from input timestamps. The hub mirror carries the flag to shell
    /// snapshots; the shell never owns it.
    lock: SessionLock,
    /// Real-time anchor of the last input event. Input stamps live on
    /// the backend event clock while idle is measured here, so each
    /// tick evaluates the lock in the input base as
    /// `last_stamp + anchor.elapsed()`.
    idle_since: Instant,
    exit: bool,
    stats: RunStats,
}

/// Control socket path for a session: alongside the Wayland socket in the
/// runtime dir, so one session owns both (`roost-<name>.control`).
pub fn control_socket_path(socket_name: &str) -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("roost-{socket_name}.control"))
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
        state.set_output_size(session.width, session.height);
        let manager = WindowManager::new(&mut state);
        let tokens = std::rc::Rc::new(TokenStore::new());
        let control_path = control_socket_path(&session.socket_name);
        let control = ControlHub::bind(control_path.clone(), tokens, crate::SEAT_NAME)
            .map_err(|e| RuntimeError::Socket(e.to_string()))?;
        let shell_policy = RestartPolicy::default();
        let shell = ShellDriver::new(
            shell_policy,
            resolve_shell_bin(session.shell_bin.as_deref()),
            session.socket_name.clone(),
            control_path,
        );
        let overlay = Overlay::new(shell_policy.max_attempts);

        let output = Output::new(
            "roost-0".to_owned(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Roost".to_owned(),
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
            shell,
            overlay,
            wallpaper: Wallpaper::new(),
            triggers: TriggerState::default(),
            lock: SessionLock::new(DEFAULT_IDLE_TIMEOUT_MS),
            idle_since: Instant::now(),
            exit: false,
            stats: RunStats::default(),
        };
        Ok((runtime, event_loop))
    }

    /// Handle one backend event. While the recovery overlay is visible
    /// it keeps focus and input (GNOME shield shape): every event goes
    /// to the overlay and windows hear nothing.
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
                    self.on_manager_input(input);
                }
            }
            WinitEvent::Redraw | WinitEvent::Focus(_) => {}
        }
    }

    /// Current time in the input-timestamp base: the newest input stamp
    /// plus real time elapsed since it arrived. The backend event clock
    /// and the system clock share no base, so the stamp anchors the
    /// base and only the gap is measured here.
    fn lock_now_ms(&self) -> u64 {
        self.lock.last_input_ms().saturating_add(
            self.idle_since
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        )
    }

    /// Whether the session is locked (hub flag: what the snapshots say).
    fn is_locked(&self) -> bool {
        self.control.is_locked()
    }

    /// Engage the lock: flag (idle machine plus hub mirror for
    /// snapshots), overview dismissed, and the exclusive lock surface
    /// up with an empty list — no window titles while locked.
    fn engage_lock(&mut self) {
        self.lock.lock();
        self.control.set_locked(true);
        self.control.set_overview(false);
        self.overlay.show(Vec::new());
    }

    /// Attempt unlock against the live daemon: verify `password` for
    /// `user` through the greeter login path and clear the lock on
    /// success (see [`crate::unlock`]). No socket configured or no
    /// daemon reachable fails closed — the session stays locked and
    /// silent; auth is never invented. Returns whether the session is
    /// unlocked afterwards.
    pub fn try_unlock(&mut self, user: &str, password: &str) -> bool {
        let Some(path) = crate::unlock::greetd_socket_path() else {
            return false;
        };
        let Ok(mut client) = GreeterClient::connect(&path) else {
            return false;
        };
        let now_ms = self.lock_now_ms();
        crate::unlock::unlock_session(
            &mut self.lock,
            &self.control,
            &mut self.overlay,
            now_ms,
            user,
            password,
            &mut client,
        )
    }

    /// Route one backend input event: consumed by the lock surface while
    /// locked (credential entry submits through [`try_unlock`](Self::try_unlock);
    /// windows hear nothing), else to the recovery overlay while it is visible,
    /// else through the overview triggers to the window manager.
    /// Trigger events still reach clients (tap toggles without
    /// breaking Super-combos); only Escape-closes is consumed, so a
    /// closing keypress never double-acts on client UI.
    fn on_manager_input(&mut self, input: ManagerInput) {
        // Every timestamped event feeds the idle accumulator first,
        // including events consumed below: activity is activity.
        self.lock.note_input(input_time(&input));
        self.idle_since = Instant::now();
        if self.is_locked() {
            return;
        }
        if !self.overlay.visible {
            if let ManagerInput::Key {
                keycode: ESCAPE_KEYCODE,
                pressed: true,
                ..
            } = input
            {
                if self.control.overview_open() {
                    self.control.set_overview(false);
                    return;
                }
            }
            let action = self.triggers.feed(
                &input,
                self.control.overview_open(),
                self.manager.pointer_pos().y,
            );
            match action {
                TriggerAction::None => {}
                TriggerAction::Toggle => self.control.set_overview(!self.control.overview_open()),
                TriggerAction::Open => self.control.set_overview(true),
            }
            self.manager.on_input(&mut self.state, input);
            // Alt-Tab drive: forward whatever the manager queued into
            // the hub broadcast; the next hub poll delivers it to the
            // shell, which owns MRU order and rendering.
            for action in self.manager.take_switcher_queue() {
                self.control.queue_switcher(action);
            }
            return;
        }
        let ManagerInput::Key {
            keycode,
            pressed: true,
            ..
        } = input
        else {
            return;
        };
        let Some(key) = overlay_key_for_keycode(keycode) else {
            return;
        };
        match self.overlay.apply_key(key) {
            None => {}
            Some(RecoveryAction::ListWindows) => self.refresh_overlay(),
            Some(RecoveryAction::RelaunchShell) => {
                self.overlay.record_restart();
                self.shell.reset_budget();
            }
            Some(RecoveryAction::ShowOverlay) => self.overlay.hide(),
        }
    }

    /// Windows currently mapped, as overlay list entries served from
    /// compositor state (no shell IPC needed).
    fn overlay_windows(&self) -> Vec<OverlayWindow> {
        self.manager
            .model()
            .snapshot()
            .windows
            .iter()
            .map(|window| OverlayWindow {
                id: window.id,
                title: window.title.clone(),
            })
            .collect()
    }

    /// Show the recovery overlay with a fresh window list, or refresh
    /// the visible list in place (selection survives when its id does).
    fn refresh_overlay(&mut self) {
        if self.overlay.visible {
            let titles: Vec<(u64, String)> = self
                .overlay_windows()
                .iter()
                .map(|window| (window.id, window.title.clone()))
                .collect();
            self.overlay.refresh_from_snapshot(&titles);
        } else {
            self.overlay.show(self.overlay_windows());
        }
    }

    /// Dispatch clients, reconcile windows, supervise the shell, produce
    /// one frame, and report whether to continue. The shell step never
    /// blocks the tick: absence shows the overlay, exhaustion stays
    /// calm, and every outcome is logged redacted (codes/counts only).
    fn tick(&mut self) -> Result<bool, RuntimeError> {
        self.display
            .dispatch_clients(&mut self.state)
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.display
            .flush_clients()
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.manager.reconcile(&mut self.state);
        let outcome = self.control.poll(self.manager.model_mut());
        for id in outcome.activated {
            self.manager.focus(&mut self.state, Some(id));
        }
        for id in outcome.closed {
            self.manager.close_window(id);
        }
        // Adopt a control-command lock (manual lock set path): the hub
        // flag flipped without the idle machine, so mirror it locally,
        // dismiss the overview, and raise the exclusive surface.
        if self.control.is_locked() && !self.lock.is_locked() {
            self.lock.lock();
            self.control.set_overview(false);
            self.overlay.show(Vec::new());
        }
        // Idle timeout from input timestamps: lock the untouched session.
        if !self.is_locked() && self.lock.check_timeout(self.lock_now_ms()) {
            self.engage_lock();
        }
        // Overview focus follows the hub flag (shell commands and
        // runtime triggers converge here); the next reconcile parks
        // or restores keyboard focus.
        self.manager.set_overview_open(self.control.overview_open());
        // While locked the overlay stays up with its empty list no
        // matter what the shell does: a shell restart while locked
        // keeps the lock screen up. Otherwise the shell step never
        // blocks the tick as before.
        if self.is_locked() {
            if !self.overlay.visible {
                self.overlay.show(Vec::new());
            }
        } else {
            match self.shell.poll(crate::state::system_millis()) {
                ShellStatus::Running => {
                    if self.overlay.visible {
                        self.overlay.hide();
                    }
                }
                ShellStatus::Waiting { .. } | ShellStatus::Fault(_) | ShellStatus::Exhausted => {
                    self.refresh_overlay()
                }
            }
        }
        for event in self.shell.drain_events() {
            eprintln!("roost-compositor: shell supervision: {event:?}");
        }
        self.stats.shell_restarts = self.shell.restarts_used();
        self.render()?;
        Ok(!self.exit)
    }

    /// Render all mapped toplevels stacked at the origin, then send frame
    /// callbacks. While the recovery overlay is visible the background
    /// shifts to a deep red (provisional overlay visual; full overlay
    /// text rendering is deferred) so the shell-absent state is
    /// unmistakable. While locked nothing beneath the lock surface may
    /// show — no windows, no layer-shell chrome (panel, notifications),
    /// no wallpaper — over a dark lock background.
    fn render(&mut self) -> Result<(), RuntimeError> {
        let size = self.backend.window_size();
        let damage = Rectangle::from_size(size);
        let locked = self.is_locked();
        let show_content = content_visible(locked);
        let background = if locked {
            Color32F::new(0.03, 0.05, 0.12, 1.0)
        } else if self.overlay.visible {
            Color32F::new(0.20, 0.08, 0.10, 1.0)
        } else {
            Color32F::new(0.08, 0.09, 0.11, 1.0)
        };
        {
            let (renderer, mut framebuffer) = self
                .backend
                .bind()
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = if show_content {
                self.manager
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
                    .collect()
            } else {
                Vec::new()
            };
            // Layer shell above windows: panel strip, then overview.
            // Hidden with everything else while locked.
            let mut elements = elements;
            if show_content {
                for (surface, (x, y), _) in crate::layer::layer_layout(&self.state) {
                    elements.extend(render_elements_from_surface_tree(
                        renderer,
                        &surface,
                        (x, y),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    ));
                }
            }
            // Settings wallpaper behind everything. Skipped under the
            // recovery overlay so the shell-absent red stays
            // unmistakable, and while locked so no content leaks.
            // Drawn in its own pass: mixing element
            // types in one list needs DMA import bounds this backend
            // does not satisfy.
            let paper = if show_content && !self.overlay.visible {
                self.wallpaper.element(renderer, size.w, size.h)
            } else {
                None
            };
            // The winit EGL surface presents bottom-up (see the Y-flip
            // in the backend's own damage path), so the output
            // transform mirrors vertically; placements stay top-down.
            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            frame
                .clear(background, &[damage])
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            if let Some(paper) = paper.as_ref() {
                draw_render_elements(&mut frame, 1.0, std::slice::from_ref(paper), &[damage])
                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
            }
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
        for (surface, _, _) in crate::layer::layer_layout(&self.state) {
            send_frames_surface_tree(&surface, &output, time, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
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
    // Graceful shutdown on SIGTERM/SIGINT: ending the loop drops the
    // runtime, whose supervisor kills the shell child (ADR 0003 kill
    // on exit). Without this a signal would bypass `Drop` and orphan
    // the shell on a dead socket. The flag write is the only work done
    // in the handler; the loop observes it below.
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, std::sync::Arc::clone(&shutdown))
            .map_err(|e| RuntimeError::Loop(format!("signal handler failed: {e}")))?;
    }
    loop {
        if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
            restore_env(prev_env);
            return Ok(runtime.stats);
        }
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

/// Input-event timestamp in the backend event base, for the session-lock
/// idle accumulator. Every [`ManagerInput`] variant carries one; axis
/// scroll counts as activity like any other event.
fn input_time(input: &ManagerInput) -> u64 {
    match *input {
        ManagerInput::Key { time, .. }
        | ManagerInput::Motion { time, .. }
        | ManagerInput::Button { time, .. }
        | ManagerInput::Axis { time, .. } => u64::from(time),
    }
}
