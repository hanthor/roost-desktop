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

use calloop::{EventLoop, LoopHandle};
use smithay::{
    backend::{
        renderer::{
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            gles::GlesRenderer,
            utils::draw_render_elements,
            Bind, Color32F, Frame, Renderer,
        },
        winit::{self, WinitEvent},
    },
    desktop::utils::send_frames_surface_tree,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::Display,
    utils::{Rectangle, Transform},
    wayland::{seat::WaylandFocus, socket::ListeningSocketSource},
};

use crate::control::{ControlHub, PeerGate};
use crate::lock::{content_visible, SessionLock, DEFAULT_IDLE_TIMEOUT_MS};

/// Idle timeout for the session lock: `ROOST_IDLE_TIMEOUT_MS` overrides
/// the five-minute default. Testability seam for scripted lock capture
/// (and lock journey tests); production runs leave it unset.
fn idle_timeout_ms() -> u64 {
    std::env::var("ROOST_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_MS)
}
use crate::overlay::{overlay_key_for_keycode, Overlay, OverlayWindow};
use crate::state::TokenStore;
use crate::supervise::{RecoveryAction, RestartPolicy, ShellDriver, ShellStatus};
use crate::wallpaper::Wallpaper;
use crate::windows::{
    translate_input, ManagerInput, TriggerAction, TriggerState, WindowManager, ESCAPE_KEYCODE,
};
use crate::xwayland::XWaylandSupervisor;
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
    /// Opt in to X11 compatibility: records the first X11 need at
    /// launch so the supervisor may spawn the server on demand.
    /// `false` (default) keeps the session native-only; the flag is
    /// the v1 trigger until per-app launch requests exist.
    pub xwayland: bool,
    /// Display backend (#52): nested window or hardware session.
    pub backend: BackendChoice,
}

/// Which display backend a session runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// Hardware when no host display exists (started from a TTY by
    /// greetd), nested otherwise.
    Auto,
    /// Nested window inside a host Wayland/X11 session.
    Winit,
    /// DRM/KMS hardware session (needs the `drm` feature).
    Drm,
}

impl BackendChoice {
    /// Parse a `--backend` value.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "auto" => Some(Self::Auto),
            "winit" | "nested" => Some(Self::Winit),
            "drm" | "kms" => Some(Self::Drm),
            _ => None,
        }
    }

    /// Resolve `Auto` against the environment: a host display means
    /// nested. Pure so the rule is testable without a display.
    pub fn resolve(self, has_host_display: bool) -> Self {
        match self {
            Self::Auto if has_host_display || !cfg!(feature = "drm") => Self::Winit,
            Self::Auto => Self::Drm,
            other => other,
        }
    }
}

/// Whether a host Wayland or X11 display is reachable from our env.
pub fn has_host_display() -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
}

/// The live display backend.
enum Backend {
    Winit(Box<winit::WinitGraphicsBackend<GlesRenderer>>),
    #[cfg(feature = "drm")]
    Drm(Box<crate::drm::DrmBackend>),
}

impl NestedSession {
    /// Session with an explicit socket name and size.
    pub fn new(socket_name: String, width: i32, height: i32) -> Self {
        Self {
            socket_name,
            width,
            height,
            shell_bin: None,
            xwayland: false,
            backend: BackendChoice::Auto,
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

/// Live nested session: display, protocol state, backend, and outputs.
/// Output handles live in the state's inventory (entry zero is the
/// primary); the runtime reaches them through [`State`] accessors.
pub struct Runtime {
    display: Display<State>,
    state: State,
    backend: Backend,
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
    /// On-demand XWayland server supervisor. Idle until the first X11
    /// need is recorded via [`Runtime::request_x11`]; the window-model
    /// join installs the real spawner, until then ticks are no-ops.
    xwayland: XWaylandSupervisor,
    /// Loop handle for event-source installation (XWayland spawn).
    /// Cloned from the owned event loop at launch; `'static`
    /// because the loop outlives the runtime in `run`.
    loop_handle: LoopHandle<'static, Runtime>,
    /// XWayland server client awaiting its `Ready` event, after
    /// which the X11 window manager starts against it (xwayland
    /// feature only).
    #[cfg(feature = "xwayland")]
    pending_x11_client: Option<smithay::reexports::wayland_server::Client>,
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
/// `None` when `XDG_RUNTIME_DIR` is unset: the privileged socket never
/// falls back to a shared temp dir (#30). [`ControlHub::bind`] further
/// refuses a runtime dir that is not private to this user.
pub fn control_socket_path(socket_name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(format!("roost-{socket_name}.control")))
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
        let event_loop = EventLoop::try_new().map_err(|e| RuntimeError::Loop(e.to_string()))?;
        let choice = session.backend.resolve(has_host_display());
        let backend = match choice {
            #[cfg(feature = "drm")]
            BackendChoice::Drm => {
                let (drm, sources) = crate::drm::DrmBackend::new()
                    .map_err(|e| RuntimeError::Backend(e.to_string()))?;
                for out in &drm.outputs {
                    let global = out.output.create_global::<State>(&dh);
                    state.add_output(&out.name, Some(out.output.clone()), out.size.w, out.size.h);
                    state.note_output_global(&out.name, global);
                }
                let handle = event_loop.handle();
                handle
                    .insert_source(sources.session, |event, _, rt: &mut Runtime| {
                        if let Backend::Drm(drm) = &mut rt.backend {
                            drm.on_session_event(event);
                        }
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                handle
                    .insert_source(sources.drm, |event, _, rt: &mut Runtime| {
                        if let Backend::Drm(drm) = &mut rt.backend {
                            drm.on_drm_event(event);
                        }
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                handle
                    .insert_source(sources.input, |event, _, rt: &mut Runtime| {
                        let inputs = match &mut rt.backend {
                            Backend::Drm(drm) => drm.translate(event),
                            Backend::Winit(_) => Vec::new(),
                        };
                        for input in inputs {
                            rt.on_manager_input(input);
                        }
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                Some(Backend::Drm(Box::new(drm)))
            }
            _ => None,
        };
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
        if backend.is_none() {
            let global = output.create_global::<State>(&dh);
            state.add_output("roost-0", Some(output), session.width, session.height);
            state.note_output_global("roost-0", global);
        }
        let manager = WindowManager::new(&mut state);
        let tokens = std::rc::Rc::new(TokenStore::new());
        let control_path = control_socket_path(&session.socket_name)
            .ok_or_else(|| RuntimeError::Socket("XDG_RUNTIME_DIR is unset".to_owned()))?;
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

        let backend = match backend {
            Some(backend) => backend,
            None => {
                let (backend, winit_loop) = winit::init::<GlesRenderer>()
                    .map_err(|e| RuntimeError::Backend(e.to_string()))?;
                event_loop
                    .handle()
                    .insert_source(winit_loop, |event, _, runtime: &mut Runtime| {
                        runtime.on_winit_event(event);
                    })
                    .map_err(|e| RuntimeError::Loop(e.to_string()))?;
                Backend::Winit(Box::new(backend))
            }
        };
        let socket = ListeningSocketSource::with_name(&session.socket_name)
            .map_err(|e| RuntimeError::Socket(e.to_string()))?;
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

        let loop_handle = event_loop.handle();
        let mut runtime = Runtime {
            display,
            state,
            backend,
            manager,
            control,
            shell,
            overlay,
            wallpaper: Wallpaper::new(),
            triggers: TriggerState::default(),
            lock: SessionLock::new(idle_timeout_ms()),
            xwayland: XWaylandSupervisor::new(),
            loop_handle,
            #[cfg(feature = "xwayland")]
            pending_x11_client: None,
            idle_since: Instant::now(),
            exit: false,
            stats: RunStats::default(),
        };
        // Session-level X11 opt-in is the recorded first X11 need:
        // the supervisor leaves `Idle` and the next tick may spawn
        // the server. Native sessions never request.
        if session.xwayland {
            runtime.request_x11();
        }
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
                if let Some(output) = self.state.primary_output() {
                    output.change_current_state(Some(mode), None, None, None);
                }
            }
            WinitEvent::CloseRequested => self.exit = true,
            WinitEvent::Input(event) => {
                let area = match &self.backend {
                    Backend::Winit(backend) => {
                        let size = backend.window_size();
                        (size.w, size.h).into()
                    }
                    #[cfg(feature = "drm")]
                    Backend::Drm(_) => return,
                };
                for input in translate_input(event, area) {
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
    /// Record the first X11 need: the supervisor leaves `Idle` and the
    /// next tick may spawn the compatibility server once its spawner
    /// is installed. Session opt-in calls this at launch; until then
    /// native sessions never request.
    pub fn request_x11(&mut self) {
        self.xwayland.request();
    }

    /// Loop handle for event-source installation (XWayland spawn).
    pub fn loop_handle(&self) -> LoopHandle<'static, Runtime> {
        self.loop_handle.clone()
    }

    /// Mutable protocol state, for the X11 handler impls.
    pub fn state_mut(&mut self) -> &mut State {
        &mut self.state
    }

    /// Take the XWayland server client awaiting its `Ready` event.
    /// (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    pub fn take_pending_x11_client(
        &mut self,
    ) -> Option<smithay::reexports::wayland_server::Client> {
        self.pending_x11_client.take()
    }

    /// Handle one XWayland server event (spawned source callback).
    /// (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    pub fn on_xwayland_event(&mut self, event: smithay::xwayland::XWaylandEvent) {
        crate::xwayland::on_xwayland_event(self, event);
    }

    fn tick(&mut self) -> Result<bool, RuntimeError> {
        self.display
            .dispatch_clients(&mut self.state)
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.display
            .flush_clients()
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.manager.reconcile(&mut self.state);
        // Publish the output inventory every tick (multi-monitor): the
        // hub broadcasts on change only, so the steady state costs one
        // short comparison. The inventory is the compositor's tracking
        // handed over as-is — never a parallel database.
        self.control.set_outputs(self.state.output_infos());
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
        // Only the supervised shell may hold a control session (#30):
        // between restarts nobody may.
        self.control.set_peer_gate(match self.shell.child_pid() {
            Some(pid) => PeerGate::Pid(pid),
            None => PeerGate::Closed,
        });
        for event in self.shell.drain_events() {
            eprintln!("roost-compositor: shell supervision: {event:?}");
        }
        // XWayland supervision rides the same tick. Without the
        // feature there is no spawner and this stays a no-op;
        // native sessions never leave `Idle`.
        #[cfg(not(feature = "xwayland"))]
        self.xwayland.tick(crate::state::system_millis(), None);
        #[cfg(feature = "xwayland")]
        {
            let display = self.display.handle();
            let loop_handle = self.loop_handle.clone();
            let pending = &mut self.pending_x11_client;
            let mut spawner = || crate::xwayland::spawn_xwayland(&display, &loop_handle, pending);
            self.xwayland
                .tick(crate::state::system_millis(), Some(&mut spawner));
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
        let locked = self.is_locked();
        let show_content = content_visible(locked);
        let overlay_visible = self.overlay.visible;
        let background = if locked {
            Color32F::new(0.03, 0.05, 0.12, 1.0)
        } else if overlay_visible {
            Color32F::new(0.20, 0.08, 0.10, 1.0)
        } else {
            Color32F::new(0.08, 0.09, 0.11, 1.0)
        };
        let show_paper = show_content && !overlay_visible;
        match &mut self.backend {
            Backend::Winit(backend) => {
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);
                {
                    let (renderer, mut framebuffer) = backend
                        .bind()
                        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                    let elements =
                        scene_elements(renderer, &self.manager, &self.state, (0, 0), show_content);
                    let paper = if show_paper {
                        self.wallpaper.element(renderer, size.w, size.h)
                    } else {
                        None
                    };
                    // The winit EGL surface presents bottom-up (see the
                    // Y-flip in the backend's own damage path), so the
                    // output transform mirrors vertically; placements
                    // stay top-down.
                    let mut frame = renderer
                        .render(&mut framebuffer, size, Transform::Flipped180)
                        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                    draw_scene(&mut frame, background, paper.as_ref(), &elements, damage)?;
                    let _ = frame
                        .finish()
                        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                }
                send_frame_callbacks(&self.state, &self.manager, self.stats.frames);
                backend
                    .submit(Some(&[damage]))
                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                self.stats.frames += 1;
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                if !drm.active {
                    return Ok(());
                }
                let pointer = drm.pointer();
                let crate::drm::DrmBackend {
                    renderer, outputs, ..
                } = &mut **drm;
                let mut queued = false;
                for out in outputs.iter_mut() {
                    // Display-paced: one frame in flight per output.
                    if out.pending {
                        continue;
                    }
                    let (mut dmabuf, _age) = match out.surface.next_buffer() {
                        Ok(buffer) => buffer,
                        Err(e) => {
                            eprintln!("roost-compositor: drm: next_buffer {}: {e}", out.name);
                            continue;
                        }
                    };
                    let size = out.size;
                    let damage = Rectangle::from_size(size);
                    let elements =
                        scene_elements(renderer, &self.manager, &self.state, out.loc, show_content);
                    let paper = if show_paper {
                        self.wallpaper.element(renderer, size.w, size.h)
                    } else {
                        None
                    };
                    let sync = {
                        let mut target = renderer
                            .bind(&mut dmabuf)
                            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                        let mut frame = renderer
                            .render(&mut target, size, Transform::Normal)
                            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                        draw_scene(&mut frame, background, paper.as_ref(), &elements, damage)?;
                        // Software pointer on top (no host cursor on
                        // bare hardware); hidden while locked.
                        if !locked {
                            let (outline, fill) = crate::drm::cursor_rects(pointer, out.loc);
                            let clip = |rects: Vec<Rectangle<i32, smithay::utils::Physical>>| {
                                rects
                                    .into_iter()
                                    .filter_map(|r| r.intersection(damage))
                                    .collect::<Vec<_>>()
                            };
                            let (outline, fill) = (clip(outline), clip(fill));
                            if !outline.is_empty() {
                                frame
                                    .clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &outline)
                                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                            }
                            if !fill.is_empty() {
                                frame
                                    .clear(Color32F::new(1.0, 1.0, 1.0, 1.0), &fill)
                                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                            }
                        }
                        frame
                            .finish()
                            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?
                    };
                    if let Err(e) = out.surface.queue_buffer(Some(sync), None, ()) {
                        eprintln!("roost-compositor: drm: queue_buffer {}: {e}", out.name);
                        continue;
                    }
                    out.pending = true;
                    queued = true;
                }
                send_frame_callbacks(&self.state, &self.manager, self.stats.frames);
                if queued {
                    self.stats.frames += 1;
                }
            }
        }
        Ok(())
    }
}

/// Window and layer-shell elements for one output whose top-left sits
/// at `offset` in the global space. Empty while content is hidden
/// (locked): nothing beneath the lock surface may show.
fn scene_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    state: &State,
    offset: (i32, i32),
    show_content: bool,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    if !show_content {
        return Vec::new();
    }
    let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = manager
        .visible_windows()
        .iter()
        .flat_map(|(window, geometry)| {
            // Unassociated X11 windows contribute no surface yet and
            // render nothing this frame.
            window
                .wl_surface()
                .map(|surface| {
                    render_elements_from_surface_tree(
                        renderer,
                        &surface,
                        (geometry.loc.x - offset.0, geometry.loc.y - offset.1),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .into_iter()
                .flatten()
        })
        .collect();
    // Layer shell above windows: panel strip, then overview.
    for (surface, (x, y), _) in crate::layer::layer_layout(state) {
        elements.extend(render_elements_from_surface_tree(
            renderer,
            &surface,
            (x - offset.0, y - offset.1),
            1.0,
            1.0,
            Kind::Unspecified,
        ));
    }
    // `elements` accumulates bottom-to-top (windows, then
    // background-to-overlay layers); Smithay 0.7 draws the first
    // element topmost.
    crate::layer::front_to_back(elements)
}

/// Clear, wallpaper (own pass: mixing element types in one list needs
/// DMA import bounds this backend does not satisfy), then the scene.
fn draw_scene(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
    background: Color32F,
    paper: Option<
        &smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>,
    >,
    elements: &[WaylandSurfaceRenderElement<GlesRenderer>],
    damage: Rectangle<i32, smithay::utils::Physical>,
) -> Result<(), RuntimeError> {
    frame
        .clear(background, &[damage])
        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    if let Some(paper) = paper {
        draw_render_elements(frame, 1.0, std::slice::from_ref(paper), &[damage])
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    }
    draw_render_elements(frame, 1.0, elements, &[damage])
        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    Ok(())
}

/// Frame callbacks for every client surface. Provisional pacing: always
/// send (zero throttle) with the primary output as scan-out; per-output
/// callbacks arrive with damage tracking.
fn send_frame_callbacks(state: &State, manager: &WindowManager, frames: u64) {
    let time = Duration::from_millis(frames.saturating_mul(16));
    let Some(output) = state.primary_output() else {
        return;
    };
    for surface in state.toplevels() {
        send_frames_surface_tree(
            surface.wl_surface(),
            &output,
            time,
            Some(Duration::ZERO),
            |_, _| Some(output.clone()),
        );
    }
    for (surface, _, _) in crate::layer::layer_layout(state) {
        send_frames_surface_tree(&surface, &output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    // X11 windows are not xdg toplevels, so the loop above never
    // reaches them; Xwayland waits on these callbacks before
    // committing content.
    #[cfg(feature = "xwayland")]
    for surface in manager.x11_surfaces() {
        send_frames_surface_tree(&surface, &output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    #[cfg(not(feature = "xwayland"))]
    let _ = manager;
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
