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
    utils::{Logical, Point, Rectangle, Transform},
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
    /// Output scale (#59): 1, or fractional like GNOME's 125, 150, 175
    /// and 200 percent. Layout works in logical pixels (physical size
    /// divided by this); clients render at it.
    pub scale: f64,
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
            scale: 1.0,
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

/// Whether a session runs X11 apps (#59): `ROOST_XWAYLAND=1` forces it,
/// `ROOST_XWAYLAND=0` turns it off, and otherwise it is on whenever an
/// `Xwayland` binary is on `PATH`, as GNOME 51 runs X11 apps
/// transparently. Builds without the `xwayland` feature never ask.
pub fn xwayland_wanted(
    env: Option<std::ffi::OsString>,
    path_var: Option<std::ffi::OsString>,
) -> bool {
    if !cfg!(feature = "xwayland") {
        return false;
    }
    match env.as_ref().and_then(|v| v.to_str()) {
        Some("1") => return true,
        Some("0") => return false,
        _ => {}
    }
    path_var
        .map(|p| std::env::split_paths(&p).any(|dir| sibling_binary(&dir, "Xwayland").is_some()))
        .unwrap_or(false)
}

#[cfg(test)]
mod xwayland_wanted_tests {
    use super::*;

    #[test]
    fn on_by_default_when_xwayland_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let path = Some(dir.path().as_os_str().to_owned());
        let enabled = cfg!(feature = "xwayland");
        assert!(!xwayland_wanted(None, path.clone()), "no Xwayland binary");
        assert_eq!(xwayland_wanted(Some("1".into()), path.clone()), enabled);
        std::fs::write(dir.path().join("Xwayland"), b"").unwrap();
        assert_eq!(xwayland_wanted(None, path.clone()), enabled);
        assert!(
            !xwayland_wanted(Some("0".into()), path.clone()),
            "opt-out wins"
        );
        assert_eq!(xwayland_wanted(Some("".into()), path), enabled);
    }
}

/// Scales GNOME offers, as a range: anything from 1 to 4.
pub fn clamp_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(1.0, 4.0)
    } else {
        1.0
    }
}

/// Logical size of a physical area at `scale` (rounded, never zero).
pub fn logical_size(width: i32, height: i32, scale: f64) -> (i32, i32) {
    (
        ((f64::from(width) / scale).round() as i32).max(1),
        ((f64::from(height) / scale).round() as i32).max(1),
    )
}

/// Where one output's pixels sit in the global logical space, and how
/// many physical pixels a logical pixel covers there.
#[derive(Debug, Clone, Copy)]
pub struct View {
    /// The output's top-left in the global logical space.
    pub offset: (i32, i32),
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl View {
    /// A global logical point in this output's physical pixels.
    pub fn physical(&self, x: f64, y: f64) -> Point<i32, smithay::utils::Physical> {
        (
            ((x - f64::from(self.offset.0)) * self.scale).round() as i32,
            ((y - f64::from(self.offset.1)) * self.scale).round() as i32,
        )
            .into()
    }
}

/// Tells the shell whether it runs nested (`nested`) or as the hardware
/// session (`hardware`).
pub const SESSION_KIND_ENV: &str = "ROOST_SESSION_KIND";

/// Shell binaries in preference order: the GTK4/libadwaita shell
/// (ADR 0006, the default), then the legacy software-drawn shell.
pub const SHELL_BINARIES: [&str; 2] = ["roost-shell-gtk", "roost-shell-host"];

/// Shell binary for a session: explicit config, then `ROOST_SHELL_BIN`,
/// then the first of [`SHELL_BINARIES`] beside this binary, then on
/// `PATH`, else the legacy name for a spawn-time lookup.
pub fn resolve_shell_bin(configured: Option<&std::path::Path>) -> std::path::PathBuf {
    resolve_shell_bin_in(
        configured,
        std::env::var_os("ROOST_SHELL_BIN"),
        current_exe_dir(),
        std::env::var_os("PATH"),
    )
}

/// [`resolve_shell_bin`] with its inputs explicit, for tests.
pub fn resolve_shell_bin_in(
    configured: Option<&std::path::Path>,
    env_bin: Option<std::ffi::OsString>,
    exe_dir: Option<std::path::PathBuf>,
    path_var: Option<std::ffi::OsString>,
) -> std::path::PathBuf {
    if let Some(path) = configured {
        return path.to_owned();
    }
    if let Some(path) = env_bin.filter(|p| !p.is_empty()) {
        return std::path::PathBuf::from(path);
    }
    for name in SHELL_BINARIES {
        if let Some(found) = exe_dir.as_deref().and_then(|dir| sibling_binary(dir, name)) {
            return found;
        }
    }
    let dirs: Vec<std::path::PathBuf> = path_var
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for name in SHELL_BINARIES {
        if let Some(found) = dirs.iter().find_map(|dir| sibling_binary(dir, name)) {
            return found;
        }
    }
    std::path::PathBuf::from(SHELL_BINARIES[1])
}

#[cfg(test)]
mod shell_bin_tests {
    use super::*;

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn gtk_shell_is_preferred_and_legacy_is_the_fallback() {
        let exe = tempfile::tempdir().unwrap();
        let path = tempfile::tempdir().unwrap();
        let exe_dir = Some(exe.path().to_owned());
        let path_var = Some(path.path().as_os_str().to_owned());
        let resolve = || resolve_shell_bin_in(None, None, exe_dir.clone(), path_var.clone());

        // Nothing installed: the legacy name, looked up at spawn.
        assert_eq!(resolve(), std::path::PathBuf::from("roost-shell-host"));
        // Only the legacy shell on PATH.
        touch(path.path(), "roost-shell-host");
        assert_eq!(resolve(), path.path().join("roost-shell-host"));
        // The GTK shell on PATH wins over the legacy one there.
        touch(path.path(), "roost-shell-gtk");
        assert_eq!(resolve(), path.path().join("roost-shell-gtk"));
        // Siblings of the compositor win over PATH, GTK first.
        touch(exe.path(), "roost-shell-host");
        assert_eq!(resolve(), exe.path().join("roost-shell-host"));
        touch(exe.path(), "roost-shell-gtk");
        assert_eq!(resolve(), exe.path().join("roost-shell-gtk"));
    }

    #[test]
    fn explicit_choices_win() {
        let exe = tempfile::tempdir().unwrap();
        touch(exe.path(), "roost-shell-gtk");
        let exe_dir = Some(exe.path().to_owned());
        assert_eq!(
            resolve_shell_bin_in(None, Some("roost-shell-host".into()), exe_dir.clone(), None),
            std::path::PathBuf::from("roost-shell-host")
        );
        assert_eq!(
            resolve_shell_bin_in(
                Some(std::path::Path::new("/x/shell")),
                None,
                exe_dir.clone(),
                None
            ),
            std::path::PathBuf::from("/x/shell")
        );
        // An empty ROOST_SHELL_BIN means unset.
        assert_eq!(
            resolve_shell_bin_in(None, Some("".into()), exe_dir, None),
            exe.path().join("roost-shell-gtk")
        );
    }
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
    /// Output scale (#59), see [`NestedSession::scale`].
    scale: f64,
    /// PipeWire, connected on the first screen cast (#61).
    pipewire: Option<crate::screencast::PipeWire>,
    /// Running screen casts.
    casts: Vec<crate::screencast::Cast>,
    /// Monitor list the Mutter D-Bus side serves.
    cast_outputs: crate::mutter::Outputs,
    /// A three-finger swipe in progress: its travel so far (#60).
    shell_swipe: Option<Point<f64, Logical>>,
    /// Overview search is showing results: the workspace card and
    /// previews hide (GNOME). Reset whenever the overview closes.
    overview_search: bool,
    /// X11 display number once XWayland's window manager is up (#59):
    /// published as `DISPLAY` to the shell for the apps it launches.
    x11_display: Option<u32>,
    /// Real-time anchor of the last input event. Input stamps live on
    /// the backend event clock while idle is measured here, so each
    /// tick evaluates the lock in the input base as
    /// `last_stamp + anchor.elapsed()`.
    idle_since: Instant,
    exit: bool,
    stats: RunStats,
    /// `ROOST_COMPOSITOR_STATE` snapshot path (journeys only).
    state_path: Option<std::path::PathBuf>,
    state_last: String,
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
                    let (lw, lh) = out.logical_size();
                    state.add_output(&out.name, Some(out.output.clone()), lw, lh);
                    state.set_output_location(&out.name, out.loc);
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
                        // The window manager has the final say (a locked
                        // pointer stays put): the drawn cursor follows it.
                        let pos = rt.manager.pointer_pos();
                        if let Backend::Drm(drm) = &mut rt.backend {
                            drm.set_pointer(pos);
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
        let scale = clamp_scale(session.scale);
        output.change_current_state(
            Some(mode),
            None,
            Some(if scale == 1.0 {
                Scale::Integer(1)
            } else {
                Scale::Fractional(scale)
            }),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        state.set_preferred_scale(scale);
        // Hardware: fractional clients render at the primary output's
        // scale from monitors.xml (#59).
        #[cfg(feature = "drm")]
        if let Some(Backend::Drm(drm)) = &backend {
            if let Some(first) = drm.outputs.first() {
                state.set_preferred_scale(first.scale);
            }
        }
        if backend.is_none() {
            let global = output.create_global::<State>(&dh);
            let (lw, lh) = logical_size(session.width, session.height, scale);
            state.add_output("roost-0", Some(output), lw, lh);
            state.note_output_global("roost-0", global);
        }
        let manager = WindowManager::new(&mut state);
        let tokens = std::rc::Rc::new(TokenStore::new());
        let control_path = control_socket_path(&session.socket_name)
            .ok_or_else(|| RuntimeError::Socket("XDG_RUNTIME_DIR is unset".to_owned()))?;
        let control = ControlHub::bind(control_path.clone(), tokens, crate::SEAT_NAME)
            .map_err(|e| RuntimeError::Socket(e.to_string()))?;
        let shell_policy = RestartPolicy::default();
        let mut shell = ShellDriver::new(
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

        // The shell offers power actions only on a hardware session: in
        // the nested preview, logind's session is the host's, so Log Out
        // must end only this compositor and Power Off must not exist.
        // An explicit ROOST_SESSION_KIND wins (proofs, debugging).
        if std::env::var_os(SESSION_KIND_ENV).is_none() {
            shell.set_env(
                SESSION_KIND_ENV,
                match backend {
                    Backend::Winit(_) => "nested",
                    Backend::Drm(_) => "hardware",
                },
            );
        }

        // linux-dmabuf lists exactly what this renderer imports (#89).
        let mut backend = backend;
        let mut state = state;
        {
            use smithay::backend::renderer::ImportDma;
            let formats: Vec<_> = match &mut backend {
                Backend::Winit(winit) => winit.renderer().dmabuf_formats().into_iter().collect(),
                Backend::Drm(drm) => drm.renderer.dmabuf_formats().into_iter().collect(),
            };
            state.enable_dmabuf(formats);
        }

        // org.gnome.Mutter.ScreenCast and DisplayConfig (#61): screen
        // sharing through the stock GNOME portal.
        let cast_outputs: crate::mutter::Outputs = Default::default();
        event_loop
            .handle()
            .insert_source(
                crate::mutter::start(cast_outputs.clone()),
                |event, _, rt: &mut Runtime| {
                    if let calloop::channel::Event::Msg(request) = event {
                        rt.on_screencast_request(request);
                    }
                },
            )
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;
        // org.gnome.Shell.Screenshot (#61): requests from the D-Bus
        // thread are answered between frames.
        event_loop
            .handle()
            .insert_source(crate::screenshot::start(), |event, _, rt: &mut Runtime| {
                if let calloop::channel::Event::Msg(request) = event {
                    let saved = rt.capture(&request.filename);
                    let _ = request.reply.send(saved);
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
            x11_display: None,
            overview_search: false,
            shell_swipe: None,
            scale: clamp_scale(session.scale),
            pipewire: None,
            casts: Vec::new(),
            cast_outputs,
            idle_since: Instant::now(),
            exit: false,
            stats: RunStats::default(),
            state_path: std::env::var_os("ROOST_COMPOSITOR_STATE")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute()),
            state_last: String::new(),
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
                let (lw, lh) = logical_size(size.w, size.h, self.scale);
                self.state.set_output_size(lw, lh);
            }
            WinitEvent::CloseRequested => self.exit = true,
            WinitEvent::Input(event) => {
                // Pointer positions land in logical pixels.
                let area = match &self.backend {
                    Backend::Winit(backend) => {
                        let size = backend.window_size();
                        logical_size(size.w, size.h, self.scale).into()
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
        let now_ms = self.lock_now_ms();
        // Under greetd, its daemon checks the password; under any other
        // login manager (GDM on Marlin), PAM does (#62). Both fail closed.
        if let Some(path) = crate::unlock::greetd_socket_path() {
            let Ok(mut client) = GreeterClient::connect(&path) else {
                return false;
            };
            return crate::unlock::unlock_session(
                &mut self.lock,
                &self.control,
                &mut self.overlay,
                now_ms,
                user,
                password,
                &mut client,
            );
        }
        let mut client = crate::pam::PamClient::new(crate::pam::service());
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
        // Three-finger swipes are the shell's (GNOME 51): consumed here,
        // acted on at the end. Other swipes reach the client.
        if self.shell_swipe_input(&input) {
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
            if self.control.overview_open() {
                if let ManagerInput::Button { pressed: true, .. } = input {
                    if self.overview_press() {
                        return;
                    }
                }
            }
            let action = self.triggers.feed(
                &input,
                self.control.overview_open(),
                self.manager.pointer_pos(),
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

    /// XWayland's window manager is up on `:display`: apps launched from
    /// now on get `DISPLAY` (#59).
    pub fn set_x11_display(&mut self, display: u32) {
        self.x11_display = Some(display);
        self.control
            .set_environment(vec![("DISPLAY".to_owned(), format!(":{display}"))]);
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

    /// Write the compositor-side state snapshot when it changed
    /// (`ROOST_COMPOSITOR_STATE`, journeys only; #65). App ids and ids,
    /// never titles.
    fn publish_state(&mut self) {
        let Some(path) = self.state_path.as_ref() else {
            return;
        };
        let model = self.manager.model();
        let snapshot = model.snapshot();
        let app_of = |id: u64| {
            snapshot
                .windows
                .iter()
                .find(|w| w.id == id)
                .and_then(|w| w.app_id.clone())
        };
        let overview_open = self.control.overview_open();
        let previews: Vec<serde_json::Value> = if overview_open {
            self.overview_layout()
                .previews
                .iter()
                .filter(|p| p.active)
                .map(|p| {
                    serde_json::json!({
                        "id": p.id,
                        "app_id": app_of(p.id),
                        "rect": [p.rect.loc.x, p.rect.loc.y, p.rect.size.w, p.rect.size.h],
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        let focused = model.focused();
        let doc = serde_json::json!({
            "x11_display": self.x11_display.map(|d| format!(":{d}")),
            "idle_timeout_ms": self.lock.timeout_ms(),
            "overview_search": self.overview_search,
            "overview_open": overview_open,
            "locked": self.is_locked(),
            "active_workspace": model.active_workspace(),
            "focused": focused,
            "focused_app_id": focused.and_then(app_of),
            "windows": self.manager.overview_windows().iter().map(|w| serde_json::json!({
                "id": w.id,
                "app_id": app_of(w.id),
                "workspace": w.workspace,
                "rect": [w.geometry.loc.x, w.geometry.loc.y, w.geometry.size.w, w.geometry.size.h],
            })).collect::<Vec<_>>(),
            "previews": previews,
            "layers": crate::layer::layer_layout(&self.state)
                .iter()
                .filter_map(|(surface, (x, y), _)| {
                    let record = self
                        .state
                        .panel_surfaces
                        .iter()
                        .find(|r| r.surface == *surface)?;
                    let size = self
                        .state
                        .layer_shell_state
                        .layer_surfaces()
                        .find(|s| s.wl_surface() == surface)
                        .and_then(|s| s.current_state().size)?;
                    Some(serde_json::json!({
                        "namespace": record.namespace,
                        "rect": [x, y, size.w, size.h],
                    }))
                })
                .collect::<Vec<_>>(),
        })
        .to_string();
        if doc == self.state_last {
            return;
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &doc).is_ok() && std::fs::rename(&tmp, path).is_ok() {
            self.state_last = doc;
        }
    }

    /// Overview scene on the primary output (#54).
    fn overview_layout(&self) -> crate::overview::OverviewLayout {
        let size = self.state.primary_size();
        let output = Rectangle::new((0, 0).into(), size);
        let model = self.manager.model();
        crate::overview::layout(
            output,
            model.workspaces(),
            model.active_workspace(),
            &self.manager.overview_windows(),
        )
    }

    /// A press while the overview is open that no shell surface took:
    /// focus the preview's window, switch to a neighbor card's
    /// workspace, or close the overview (GNOME shape). Returns whether
    /// the press was consumed.
    fn overview_press(&mut self) -> bool {
        let pos = self.manager.pointer_pos();
        if crate::layer::topmost_layer_at(&self.state, pos.x as i32, pos.y as i32).is_some()
            || self.manager.popup_at(&self.state, pos).is_some()
        {
            return false;
        }
        // Search hides the cards: a press beside the results dismisses.
        if self.overview_search {
            self.control.set_overview(false);
            return true;
        }
        match crate::overview::hit(&self.overview_layout(), pos) {
            crate::overview::OverviewHit::Window(id) => {
                self.manager.focus(&mut self.state, Some(id));
                self.control.set_overview(false);
            }
            crate::overview::OverviewHit::Workspace(ws) => {
                self.manager.switch_to_workspace(&mut self.state, ws);
            }
            crate::overview::OverviewHit::Dismiss => self.control.set_overview(false),
        }
        true
    }

    /// Track a three-finger swipe; returns whether `input` was one of
    /// its phases (and so consumed).
    fn shell_swipe_input(&mut self, input: &ManagerInput) -> bool {
        match *input {
            ManagerInput::SwipeBegin { fingers, .. } => {
                if fingers == crate::windows::SHELL_SWIPE_FINGERS {
                    self.shell_swipe = Some(Point::from((0.0, 0.0)));
                    return true;
                }
                false
            }
            ManagerInput::SwipeUpdate { delta, .. } => match self.shell_swipe.as_mut() {
                Some(travel) => {
                    *travel += delta;
                    true
                }
                None => false,
            },
            ManagerInput::SwipeEnd { cancelled, .. } => {
                let Some(travel) = self.shell_swipe.take() else {
                    return false;
                };
                if !cancelled {
                    use crate::windows::SwipeAction;
                    match crate::windows::swipe_action(travel.x, travel.y) {
                        Some(SwipeAction::OpenOverview) => self.control.set_overview(true),
                        Some(SwipeAction::CloseOverview) => self.control.set_overview(false),
                        Some(SwipeAction::NextWorkspace) => {
                            self.manager.switch_relative(&mut self.state, 1);
                        }
                        Some(SwipeAction::PreviousWorkspace) => {
                            self.manager.switch_relative(&mut self.state, -1);
                        }
                        None => {}
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Render the primary output's current scene offscreen and save it
    /// as a PNG (`requested` when absolute, else GNOME's default path).
    /// Never while locked: nothing behind the lock may leave the session.
    pub fn capture(&mut self, requested: &std::path::Path) -> Option<std::path::PathBuf> {
        if self.is_locked() {
            return None;
        }
        let path =
            crate::screenshot::target_path(requested, &crate::screenshot::jiff_like::Stamp::now())?;
        let (w, h, rgba) =
            self.render_pixels(None, smithay::backend::allocator::Fourcc::Abgr8888)?;
        crate::screenshot::save_png(&path, w as u32, h as u32, &rgba).ok()?;
        eprintln!("roost-compositor: screenshot saved to {}", path.display());
        Some(path)
    }

    /// Render one output's current scene (the primary, or `connector`)
    /// offscreen and read it back in `fourcc` byte order (Abgr8888 is
    /// RGBA in memory, Xrgb8888 is BGRx). Never while locked.
    fn render_pixels(
        &mut self,
        connector: Option<&str>,
        fourcc: smithay::backend::allocator::Fourcc,
    ) -> Option<(i32, i32, Vec<u8>)> {
        use smithay::backend::renderer::{ExportMem, Offscreen};
        if self.is_locked() {
            return None;
        }
        let overview = self.control.overview_open().then(|| self.overview_layout());
        let cards = overview.as_ref().filter(|_| !self.overview_search);
        let decor_global = cards
            .map(|layout| overview_decor(layout, self.manager.pointer_pos()))
            .unwrap_or_default();
        let background = if overview.is_some() {
            OVERVIEW_BACKGROUND
        } else {
            Color32F::new(0.08, 0.09, 0.11, 1.0)
        };
        let (renderer, size, view) = match &mut self.backend {
            Backend::Winit(backend) => {
                let size = backend.window_size();
                let view = View {
                    offset: (0, 0),
                    scale: self.scale,
                };
                (backend.renderer(), size, view)
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                let crate::drm::DrmBackend {
                    renderer, outputs, ..
                } = &mut **drm;
                let out = match connector {
                    Some(name) => outputs.iter().find(|o| o.name == name)?,
                    None => outputs.first()?,
                };
                let view = View {
                    offset: out.loc,
                    scale: out.scale,
                };
                (renderer, out.size, view)
            }
        };
        let buffer_size = (size.w, size.h).into();
        let mut texture: smithay::backend::renderer::gles::GlesTexture =
            renderer.create_buffer(fourcc, buffer_size).ok()?;
        let elements = scene_elements(
            renderer,
            &self.manager,
            &self.state,
            view,
            true,
            overview.as_ref(),
        );
        let decor = decor_for_output(&decor_global, view);
        let previews = preview_elements(renderer, &self.manager, view, cards);
        let paper = overview
            .is_none()
            .then(|| self.wallpaper.element(renderer, size.w, size.h))
            .flatten();
        let damage = Rectangle::from_size(size);
        let mut target = renderer.bind(&mut texture).ok()?;
        {
            let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
            draw_scene(
                &mut frame,
                background,
                paper.as_ref(),
                &elements,
                Target {
                    damage,
                    scale: view.scale,
                },
                &decor,
                &previews,
            )
            .ok()?;
            let _ = frame.finish().ok()?;
        }
        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size(buffer_size), fourcc)
            .ok()?;
        let bytes = renderer.map_texture(&mapping).ok()?.to_vec();
        Some((size.w, size.h, bytes))
    }

    /// Screen-cast work for this tick (#61): start and stop casts the
    /// D-Bus side asked for, and feed every streaming cast a frame.
    fn screencast_tick(&mut self) {
        self.casts.retain(|cast| !cast.failed());
        let wanted: Vec<(usize, String)> = self
            .casts
            .iter()
            .enumerate()
            .filter(|(_, cast)| cast.wants_frame().is_some())
            .map(|(i, cast)| (i, cast.connector.clone()))
            .collect();
        for (index, connector) in wanted {
            let target = match &self.backend {
                Backend::Winit(_) => None,
                #[cfg(feature = "drm")]
                Backend::Drm(_) => Some(connector.as_str()),
            };
            let Some((w, h, bgrx)) =
                self.render_pixels(target, smithay::backend::allocator::Fourcc::Xrgb8888)
            else {
                continue;
            };
            if let Some(cast) = self.casts.get_mut(index) {
                cast.send_frame(w, h, &bgrx);
            }
        }
    }

    /// One D-Bus screen-cast request.
    fn on_screencast_request(&mut self, request: crate::mutter::ToLoop) {
        match request {
            crate::mutter::ToLoop::StartCast {
                session_id,
                connector,
                signal,
            } => {
                if self.pipewire.is_none() {
                    self.pipewire = crate::screencast::PipeWire::new(&self.loop_handle);
                    if self.pipewire.is_none() {
                        eprintln!("roost-compositor: screen cast: PipeWire is not running");
                        crate::mutter::session_closed(&signal);
                        return;
                    }
                }
                let Some(snapshot) = self
                    .cast_outputs
                    .lock()
                    .ok()
                    .and_then(|o| o.iter().find(|o| o.connector == connector).cloned())
                else {
                    return;
                };
                let Some(pw) = self.pipewire.as_ref() else {
                    return;
                };
                match pw.start_cast(
                    session_id,
                    connector,
                    snapshot.width,
                    snapshot.height,
                    signal,
                ) {
                    Some(cast) => self.casts.push(cast),
                    None => eprintln!("roost-compositor: screen cast: stream failed to start"),
                }
            }
            crate::mutter::ToLoop::StopCast { session_id } => {
                self.casts.retain(|cast| cast.session_id != session_id);
            }
        }
    }

    /// Keep the D-Bus side's monitor list current (cheap when unchanged).
    fn publish_cast_outputs(&self) {
        let snapshot: Vec<crate::mutter::OutputSnapshot> = self
            .state
            .output_entries()
            .into_iter()
            .filter_map(|(name, output, loc, primary)| {
                let mode = output.current_mode()?;
                Some(crate::mutter::OutputSnapshot {
                    connector: name,
                    make: output.physical_properties().make,
                    model: output.physical_properties().model,
                    width: mode.size.w,
                    height: mode.size.h,
                    refresh_mhz: mode.refresh,
                    x: loc.0,
                    y: loc.1,
                    scale: output.current_scale().fractional_scale(),
                    primary,
                })
            })
            .collect();
        if let Ok(mut current) = self.cast_outputs.lock() {
            if *current != snapshot {
                *current = snapshot;
            }
        }
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
        if let Some(active) = outcome.overview_search {
            self.overview_search = active;
        }
        if !self.control.overview_open() {
            self.overview_search = false;
        }
        // GNOME's idle and lock settings, from the shell (#63).
        if let Some(ms) = outcome.idle_timeout {
            self.lock.set_timeout(if ms == 0 { u64::MAX } else { ms });
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
        // A live idle inhibitor (video, presentation) counts as activity.
        if self.state.idle_inhibited() {
            let now = self.lock_now_ms();
            self.lock.note_input(now);
            self.idle_since = Instant::now();
        }
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
        self.publish_state();
        self.publish_cast_outputs();
        self.render()?;
        self.screencast_tick();
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
        let overview = (show_content && !overlay_visible && self.control.overview_open())
            .then(|| self.overview_layout());
        // While search shows results, the workspace view steps aside.
        let cards = overview.as_ref().filter(|_| !self.overview_search);
        let decor_global = cards
            .map(|layout| overview_decor(layout, self.manager.pointer_pos()))
            .unwrap_or_default();
        let background = if overview.is_some() {
            OVERVIEW_BACKGROUND
        } else {
            background
        };
        let show_paper = show_content && !overlay_visible && overview.is_none();
        match &mut self.backend {
            Backend::Winit(backend) => {
                let size = backend.window_size();
                let damage = Rectangle::from_size(size);
                {
                    let (renderer, mut framebuffer) = backend
                        .bind()
                        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                    let view = View {
                        offset: (0, 0),
                        scale: self.scale,
                    };
                    let elements = scene_elements(
                        renderer,
                        &self.manager,
                        &self.state,
                        view,
                        show_content,
                        overview.as_ref(),
                    );
                    let decor = decor_for_output(&decor_global, view);
                    let previews = preview_elements(renderer, &self.manager, view, cards);
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
                    draw_scene(
                        &mut frame,
                        background,
                        paper.as_ref(),
                        &elements,
                        Target {
                            damage,
                            scale: view.scale,
                        },
                        &decor,
                        &previews,
                    )?;
                    let _ = frame
                        .finish()
                        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                }
                send_surface_scales(&self.state, &self.manager);
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
                    // Per-output scale from GNOME's monitors.xml (#59).
                    let view = View {
                        offset: out.loc,
                        scale: out.scale,
                    };
                    let elements = scene_elements(
                        renderer,
                        &self.manager,
                        &self.state,
                        view,
                        show_content,
                        overview.as_ref(),
                    );
                    let decor = decor_for_output(&decor_global, view);
                    let previews = preview_elements(renderer, &self.manager, view, cards);
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
                        draw_scene(
                            &mut frame,
                            background,
                            paper.as_ref(),
                            &elements,
                            Target {
                                damage,
                                scale: view.scale,
                            },
                            &decor,
                            &previews,
                        )?;
                        // Software pointer on top (no host cursor on
                        // bare hardware); hidden while locked.
                        if !locked {
                            let (outline, fill) =
                                crate::drm::cursor_rects(pointer, out.loc, out.scale);
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
                send_surface_scales(&self.state, &self.manager);
                send_frame_callbacks(&self.state, &self.manager, self.stats.frames);
                if queued {
                    self.stats.frames += 1;
                }
            }
        }
        Ok(())
    }
}

/// Overview backdrop (GNOME 51's dark grey behind the cards).
const OVERVIEW_BACKGROUND: Color32F = Color32F::new(0.14, 0.14, 0.15, 1.0);
/// Workspace card fill (the desktop seen through the card).
const OVERVIEW_CARD: Color32F = Color32F::new(0.08, 0.09, 0.11, 1.0);
/// Hover ring around the preview under the pointer.
const OVERVIEW_HOVER: Color32F = Color32F::new(0.62, 0.66, 0.72, 1.0);
/// Hover ring thickness.
const HOVER_RING: i32 = 4;

/// Overview solid shapes in the global space: card fills, then the ring
/// around the active preview under `pointer`.
fn overview_decor(
    layout: &crate::overview::OverviewLayout,
    pointer: Point<f64, Logical>,
) -> Vec<(Color32F, Vec<Rectangle<i32, Logical>>)> {
    let cards = layout.cards.iter().map(|c| c.rect).collect();
    let mut out = vec![(OVERVIEW_CARD, cards)];
    if let Some(p) = layout
        .previews
        .iter()
        .rev()
        .find(|p| p.active && p.rect.to_f64().contains(pointer))
    {
        let r = p.rect;
        let t = HOVER_RING;
        let ring = vec![
            Rectangle::new(
                (r.loc.x - t, r.loc.y - t).into(),
                (r.size.w + 2 * t, t).into(),
            ),
            Rectangle::new(
                (r.loc.x - t, r.loc.y + r.size.h).into(),
                (r.size.w + 2 * t, t).into(),
            ),
            Rectangle::new((r.loc.x - t, r.loc.y).into(), (t, r.size.h).into()),
            Rectangle::new((r.loc.x + r.size.w, r.loc.y).into(), (t, r.size.h).into()),
        ];
        out.push((OVERVIEW_HOVER, ring));
    }
    out
}

/// Global decor in one output's physical pixels.
fn decor_for_output(
    decor: &[(Color32F, Vec<Rectangle<i32, Logical>>)],
    view: View,
) -> Vec<(Color32F, Vec<Rectangle<i32, smithay::utils::Physical>>)> {
    decor
        .iter()
        .map(|(color, rects)| {
            (
                *color,
                rects
                    .iter()
                    .map(|r| {
                        let top_left = view.physical(f64::from(r.loc.x), f64::from(r.loc.y));
                        let bottom_right = view
                            .physical(f64::from(r.loc.x + r.size.w), f64::from(r.loc.y + r.size.h));
                        Rectangle::from_extremities(top_left, bottom_right)
                    })
                    .collect(),
            )
        })
        .collect()
}

/// A window preview element: the window's surface tree, shrunk about
/// its top-left corner.
type PreviewElement = smithay::backend::renderer::element::utils::RescaleRenderElement<
    WaylandSurfaceRenderElement<GlesRenderer>,
>;

/// Overview previews for one output, front to back (#54).
fn preview_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    view: View,
    overview: Option<&crate::overview::OverviewLayout>,
) -> Vec<PreviewElement> {
    let Some(layout) = overview else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for preview in &layout.previews {
        let Some(surface) = manager.surface_of(preview.id) else {
            continue;
        };
        // Shrink about the surface origin, placed so the visible window
        // (inside client shadows, also shrunk) fills the preview rect.
        let geo = crate::popup::window_geometry_loc(&surface);
        let origin = view.physical(
            f64::from(preview.rect.loc.x) - f64::from(geo.x) * preview.scale,
            f64::from(preview.rect.loc.y) - f64::from(geo.y) * preview.scale,
        );
        for element in render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
            renderer,
            &surface,
            origin,
            view.scale,
            1.0,
            Kind::Unspecified,
        ) {
            out.push(
                smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                    element,
                    origin,
                    preview.scale,
                ),
            );
        }
    }
    crate::layer::front_to_back(out)
}

/// Window and layer-shell elements for one output whose top-left sits
/// at `offset` in the global space. Empty while content is hidden
/// (locked): nothing beneath the lock surface may show.
fn scene_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    state: &State,
    view: View,
    show_content: bool,
    overview: Option<&crate::overview::OverviewLayout>,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    if !show_content {
        return Vec::new();
    }
    if overview.is_some() {
        // Overview (#54): windows are drawn as rescaled previews in
        // their own pass (`preview_elements`); here only the shell's
        // layers (and their popovers) go on top.
        let mut elements = Vec::new();
        for (surface, (x, y), _) in crate::layer::layer_layout(state) {
            elements.extend(render_elements_from_surface_tree(
                renderer,
                &surface,
                view.physical(f64::from(x), f64::from(y)),
                view.scale,
                1.0,
                Kind::Unspecified,
            ));
            for popup in crate::popup::placed_popups(&surface, (x, y).into(), false) {
                elements.extend(render_elements_from_surface_tree(
                    renderer,
                    &popup.surface,
                    view.physical(f64::from(popup.origin.x), f64::from(popup.origin.y)),
                    view.scale,
                    1.0,
                    Kind::Unspecified,
                ));
            }
        }
        return crate::layer::front_to_back(elements);
    }
    let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
    let tree = |renderer: &mut GlesRenderer,
                elements: &mut Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
                surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                at: Point<i32, Logical>| {
        elements.extend(render_elements_from_surface_tree(
            renderer,
            surface,
            view.physical(f64::from(at.x), f64::from(at.y)),
            view.scale,
            1.0,
            Kind::Unspecified,
        ));
    };
    for (window, geometry) in manager.visible_windows() {
        // Unassociated X11 windows contribute no surface yet and
        // render nothing this frame.
        if let Some(surface) = window.wl_surface() {
            let origin = crate::popup::surface_origin(&surface, geometry.loc);
            tree(renderer, &mut elements, &surface, origin);
            // Popups (#88) right above their window.
            for popup in crate::popup::placed_popups(&surface, origin, true) {
                tree(renderer, &mut elements, &popup.surface, popup.origin);
            }
        }
    }
    // Layer shell above windows: panel strip, then overview, each with
    // its popups (toolkit popovers hang off layer surfaces).
    for (surface, (x, y), _) in crate::layer::layer_layout(state) {
        tree(renderer, &mut elements, &surface, (x, y).into());
        for popup in crate::popup::placed_popups(&surface, (x, y).into(), false) {
            tree(renderer, &mut elements, &popup.surface, popup.origin);
        }
    }
    // `elements` accumulates bottom-to-top (windows, then
    // background-to-overlay layers); Smithay 0.7 draws the first
    // element topmost.
    crate::layer::front_to_back(elements)
}

/// Clear, wallpaper (own pass: mixing element types in one list needs
/// DMA import bounds this backend does not satisfy), then the scene.
/// What one frame redraws, and at which scale.
#[derive(Debug, Clone, Copy)]
struct Target {
    damage: Rectangle<i32, smithay::utils::Physical>,
    scale: f64,
}

fn draw_scene(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
    background: Color32F,
    paper: Option<
        &smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>,
    >,
    elements: &[WaylandSurfaceRenderElement<GlesRenderer>],
    target: Target,
    decor: &[(Color32F, Vec<Rectangle<i32, smithay::utils::Physical>>)],
    previews: &[PreviewElement],
) -> Result<(), RuntimeError> {
    let Target { damage, scale } = target;
    frame
        .clear(background, &[damage])
        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    if let Some(paper) = paper {
        draw_render_elements(frame, 1.0, std::slice::from_ref(paper), &[damage])
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    }
    // Solid shapes under the surfaces (overview cards, hover ring).
    for (color, rects) in decor {
        let rects: Vec<_> = rects
            .iter()
            .filter_map(|r| r.intersection(damage))
            .collect();
        if !rects.is_empty() {
            frame
                .clear(*color, &rects)
                .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        }
    }
    // Surfaces carry logical sizes: draw them at the output scale. The
    // wallpaper above is already sized in physical pixels.
    if !previews.is_empty() {
        draw_render_elements(frame, scale, previews, &[damage])
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    }
    draw_render_elements(frame, scale, elements, &[damage])
        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    Ok(())
}

/// Frame callbacks for every client surface. Provisional pacing: always
/// send (zero throttle) with the primary output as scan-out; per-output
/// callbacks arrive with damage tracking.
/// Tell every window and layer surface the scale of the output it is on
/// (preferred buffer scale and fractional scale), so on mixed-DPI setups
/// each renders sharp for its own monitor. Smithay only sends changes.
/// niri's `send_scale_transform` shape (#59).
fn send_surface_scales(state: &State, manager: &WindowManager) {
    use smithay::wayland::compositor::{
        send_surface_state, with_surface_tree_downward, TraversalAction,
    };
    use smithay::wayland::fractional_scale::with_fractional_scale;
    let send = |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                scale: smithay::output::Scale| {
        with_surface_tree_downward(
            surface,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |surface, data, _| {
                send_surface_state(surface, data, scale.integer_scale(), Transform::Normal);
                with_fractional_scale(data, |fractional| {
                    fractional.set_preferred_scale(scale.fractional_scale());
                });
            },
            |_, _, _| true,
        );
    };
    for (window, geometry) in manager.visible_windows() {
        if let Some(surface) = window.wl_surface() {
            send(&surface, state.scale_for(geometry));
        }
    }
    for (surface, (x, y), _) in crate::layer::layer_layout(state) {
        send(
            &surface,
            state.scale_for(Rectangle::new((x, y).into(), (1, 1).into())),
        );
    }
}

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
    // Popups commit on frame callbacks like any surface (#88).
    let parents = state
        .toplevels()
        .iter()
        .map(|t| t.wl_surface().clone())
        .chain(
            crate::layer::layer_layout(state)
                .into_iter()
                .map(|(s, _, _)| s),
        )
        .collect::<Vec<_>>();
    for parent in parents {
        for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(&parent) {
            send_frames_surface_tree(
                popup.wl_surface(),
                &output,
                time,
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
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
        | ManagerInput::Axis { time, .. }
        | ManagerInput::SwipeBegin { time, .. }
        | ManagerInput::SwipeUpdate { time, .. }
        | ManagerInput::SwipeEnd { time, .. } => u64::from(time),
        ManagerInput::RelativeMotion { utime, .. } => utime / 1000,
    }
}
