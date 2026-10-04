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
    /// Open the overview when the session starts, as GNOME Shell does at
    /// login. `roost-session` (real logins) sets it; developer and proof
    /// runs start on the desktop.
    pub startup_overview: bool,
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
            startup_overview: false,
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
    /// The IBus bridge, when IBus is installed.
    ime: Option<crate::ime::ImeBridge>,
    overlay: Overlay,
    wallpaper: Wallpaper,
    triggers: TriggerState,
    /// Compositor-owned session lock: flag plus idle accumulator fed
    /// from input timestamps. The hub mirror carries the flag to shell
    /// snapshots; the shell never owns it.
    lock: SessionLock,
    blank: crate::lock::IdleBlank,
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
    #[cfg(feature = "xwayland")]
    x11_sockets: Option<smithay::xwayland::XWaylandSockets>,
    #[cfg(feature = "xwayland")]
    x11_watchers: Vec<calloop::RegistrationToken>,
    #[cfg(feature = "xwayland")]
    x11_source: Option<calloop::RegistrationToken>,
    #[cfg(feature = "xwayland")]
    active_x11_client: Option<smithay::reexports::wayland_server::Client>,
    #[cfg(feature = "xwayland")]
    x11_failed: bool,

    /// Output scale (#59), see [`NestedSession::scale`].
    scale: f64,
    /// GNOME's input settings as last applied (#60).
    input_settings: roost_shell_control::InputSettings,
    /// PipeWire, connected on the first screen cast (#61).
    pipewire: Option<crate::screencast::PipeWire>,
    /// Running screen casts.
    casts: Vec<crate::screencast::Cast>,
    /// Monitor list the Mutter D-Bus side serves.
    cast_outputs: crate::mutter::Outputs,
    /// Window list org.gnome.Shell.Introspect serves (window sharing).
    introspect: crate::introspect::Handle,
    /// A three-finger swipe in progress: its travel so far (#60).
    shell_swipe: Option<Point<f64, Logical>>,
    /// The overview transition: 0 the desktop, 1 the overview (linear
    /// time; drawn eased), moving toward the open state each frame.
    overview_progress: f64,
    overview_progress_at: Instant,
    tile_animation: Option<(u64, crate::animation::EaseRect, Instant)>,
    /// Last strip-view spring step, for the per-frame time delta.
    strip_view_at: Instant,
    /// A three-finger vertical swipe driving the transition: the
    /// progress it started from.
    overview_swipe_from: Option<f64>,
    /// Overview search is showing results: the workspace card and
    /// previews hide (GNOME). Reset whenever the overview closes.
    overview_search: bool,
    /// The overview shows the app grid: workspaces become thumbnails
    /// along the top (GNOME's app grid state). Reset on close.
    overview_app_grid: bool,
    /// A press on an overview preview: the window and where it was
    /// grabbed, and whether it has become a drag (GNOME DnD).
    overview_drag: Option<(u64, Point<f64, Logical>, bool)>,
    /// The Alt+Tab switcher's window thumbnails (frames from the shell).
    switcher_thumbnails: Vec<roost_shell_control::SwitcherThumbnail>,
    /// Reserved X11 display, advertised before XWayland starts (#219).
    /// Actual window-manager readiness is independently state.xwm.is_some().
    x11_display: Option<u32>,
    /// Real-time anchor of the last input event. Input stamps live on
    /// the backend event clock while idle is measured here, so each
    /// tick evaluates the lock in the input base as
    /// `last_stamp + anchor.elapsed()`.
    idle_since: Instant,
    /// Lock-screen password checks report back here.
    unlock_results: calloop::channel::Sender<(u64, bool)>,
    /// A password check is running.
    unlock_inflight: bool,
    /// org.gnome.Mutter.IdleMonitor.
    idle_monitor: crate::idle_monitor::IdleMonitor,
    exit: bool,
    stats: RunStats,
    /// `ROOST_COMPOSITOR_STATE` snapshot path (journeys only).
    state_path: Option<std::path::PathBuf>,
    state_last: String,
    /// Opt-in synthetic touchpad phases for the nested proof harness.
    proof_swipe_last: String,
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

impl Drop for Runtime {
    fn drop(&mut self) {
        #[cfg(feature = "xwayland")]
        {
            // Event sources can retain loop handles after dispatch stops.
            // Release our owned sources explicitly so XWayland's display
            // lock/socket owner is dropped before the compositor exits.
            for token in self.x11_watchers.drain(..) {
                self.loop_handle.remove(token);
            }
            if let Some(token) = self.x11_source.take() {
                self.loop_handle.remove(token);
            }
            self.x11_sockets = None;
            self.state.xwm = None;
            self.pending_x11_client = None;
            self.active_x11_client = None;
        }
    }
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
                    .insert_source(sources.drm, |event, metadata, rt: &mut Runtime| {
                        let flip = match &mut rt.backend {
                            Backend::Drm(drm) => drm.on_drm_event(event, metadata),
                            Backend::Winit(_) => None,
                        };
                        if let Some(flip) = flip {
                            rt.page_flipped(flip);
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
        // org.gnome.Shell.Introspect: the portal's window picker.
        let introspect = crate::introspect::start(cast_outputs.clone());
        event_loop
            .handle()
            .insert_source(
                crate::mutter::start(cast_outputs.clone(), introspect.windows.clone()),
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
                use crate::screenshot::Request;
                match event {
                    calloop::channel::Event::Msg(Request::Shot {
                        filename,
                        window,
                        reply,
                    }) => {
                        let saved = if window {
                            rt.capture_window(&filename)
                        } else {
                            rt.capture(&filename)
                        };
                        let _ = reply.send(saved);
                    }
                    calloop::channel::Event::Msg(Request::Windows { directory, reply }) => {
                        let _ = reply.send(rt.capture_selector_windows(&directory));
                    }
                    calloop::channel::Event::Closed => {}
                }
            })
            .map_err(|e| RuntimeError::Loop(e.to_string()))?;

        // Lock-screen password checks finish here, between frames.
        let (unlock_results, unlock_source) = calloop::channel::channel::<(u64, bool)>();
        event_loop
            .handle()
            .insert_source(unlock_source, |event, _, rt: &mut Runtime| {
                if let calloop::channel::Event::Msg((request, ok)) = event {
                    rt.finish_unlock(request, ok);
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
            ime: crate::ime::ImeBridge::configured(&session.socket_name),
            overlay,
            wallpaper: Wallpaper::new(),
            triggers: TriggerState::default(),
            lock: SessionLock::new(idle_timeout_ms()),
            blank: crate::lock::IdleBlank::default(),
            xwayland: XWaylandSupervisor::new(),
            loop_handle,
            #[cfg(feature = "xwayland")]
            pending_x11_client: None,
            #[cfg(feature = "xwayland")]
            x11_sockets: None,
            #[cfg(feature = "xwayland")]
            x11_watchers: Vec::new(),
            #[cfg(feature = "xwayland")]
            x11_source: None,
            #[cfg(feature = "xwayland")]
            active_x11_client: None,
            #[cfg(feature = "xwayland")]
            x11_failed: false,
            x11_display: None,
            overview_search: false,
            overview_app_grid: false,
            overview_drag: None,
            switcher_thumbnails: Vec::new(),
            shell_swipe: None,
            overview_progress: 0.0,
            overview_progress_at: Instant::now(),
            tile_animation: None,
            strip_view_at: Instant::now(),
            overview_swipe_from: None,
            scale: clamp_scale(session.scale),
            input_settings: Default::default(),
            pipewire: None,
            casts: Vec::new(),
            cast_outputs,
            introspect,
            idle_since: Instant::now(),
            unlock_results,
            unlock_inflight: false,
            idle_monitor: crate::idle_monitor::start(),
            exit: false,
            stats: RunStats::default(),
            state_path: std::env::var_os("ROOST_COMPOSITOR_STATE")
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute()),
            state_last: String::new(),
            proof_swipe_last: String::new(),
        };
        // Reserve and advertise sockets, but spawn only when a real X11
        // client connects. Native clients do not start a compatibility process.
        #[cfg(feature = "xwayland")]
        if session.xwayland {
            if let Err(error) = runtime.prepare_x11(None) {
                eprintln!("roost-compositor: xwayland: socket preparation failed: {error}; native session continues");
            }
        }
        // GNOME Shell greets a login with the overview.
        if session.startup_overview {
            runtime.control.set_overview(true);
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

    fn blank_alpha(&self) -> f32 {
        self.blank.alpha(self.lock.idle_ms(self.lock_now_ms()))
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

    /// A lock-screen password check finished: a verified password
    /// clears the lock (the shell then destroys its lock surfaces), and
    /// the shell hears the result either way.
    fn finish_unlock(&mut self, request: u64, ok: bool) {
        self.unlock_inflight = false;
        if std::env::var_os("ROOST_LOCK_TRACE").is_some() {
            eprintln!("roost-compositor: lock authentication finished accepted={ok}");
        }
        if ok && self.is_locked() {
            self.lock.unlock(self.lock_now_ms());
            self.control.set_locked(false);
            self.overlay.hide();
        }
        self.control.finish_unlock(request, ok);
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
        let waking_blank = self.blank_alpha() > 0.0;
        self.lock.note_input(input_time(&input));
        self.idle_since = Instant::now();
        self.idle_monitor.activity();
        if self.is_locked() {
            // Only the lock screen hears input; with none up (shell
            // gone), nothing does.
            let surface = self
                .state
                .primary_output_name()
                .and_then(|name| self.state.lock_surface_for(&name));
            if let Some(surface) = surface {
                self.manager.lock_input(&mut self.state, &surface, input);
            }
            // Media keys still work on the lock screen, as in GNOME.
            for (action, time, mode) in self.manager.take_accelerators_fired() {
                self.control.queue_accelerator(action, time, mode);
            }
            return;
        }
        if waking_blank {
            // Activity cancels the idle shield; the wake event belongs to
            // that shield, not the previously focused application.
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
            } else {
                self.overview_drag = None;
            }
            if self.overview_drag.is_some() {
                match input {
                    ManagerInput::Button { pressed: false, .. } => {
                        // The press never reached a client: neither
                        // does its release.
                        self.overview_release();
                        return;
                    }
                    ManagerInput::Motion { pos, .. } => {
                        if let Some((_, start, dragging)) = &mut self.overview_drag {
                            let (dx, dy) = (pos.x - start.x, pos.y - start.y);
                            if dx.hypot(dy) > crate::overview::DRAG_THRESHOLD {
                                *dragging = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Super's tap belongs to a window inhibiting shortcuts
            // (keyboard-shortcuts-inhibit, #89).
            let inhibited =
                matches!(input, ManagerInput::Key { .. }) && self.state.shortcuts_inhibited();
            let action = if inhibited {
                TriggerAction::None
            } else {
                self.triggers.feed(
                    &input,
                    self.control.overview_open(),
                    self.manager.pointer_pos(),
                )
            };
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
                // A closing switcher takes its thumbnails with it.
                if matches!(
                    action,
                    roost_shell_control::SwitcherAction::Commit
                        | roost_shell_control::SwitcherAction::Cancel
                ) {
                    self.switcher_thumbnails.clear();
                }
                self.control.queue_switcher(action);
            }
            for (action, time, mode) in self.manager.take_accelerators_fired() {
                self.control.queue_accelerator(action, time, mode);
            }
            for (index, count) in self.manager.take_workspace_popups() {
                self.control
                    .queue_message(roost_shell_control::Message::WorkspacePopup { index, count });
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

    /// Advertise the reserved display to apps, independently of XWM readiness.
    pub fn set_x11_display(&mut self, display: u32) {
        self.x11_display = Some(display);
        if let Some(ime) = &mut self.ime {
            ime.set_x11_display(display);
        }
        self.control
            .set_environment(vec![("DISPLAY".to_owned(), format!(":{display}"))]);
    }

    #[cfg(feature = "xwayland")]
    fn prepare_x11(&mut self, display: Option<u32>) -> Result<(), Box<dyn std::error::Error>> {
        let sockets = smithay::xwayland::XWaylandSockets::prepare(display, true)?;
        let number = sockets.display_number();
        let handles = sockets
            .listen_fds()
            .map(|fd| fd.try_clone_to_owned())
            .collect::<Result<Vec<_>, _>>()?;
        let mut watchers = Vec::new();
        for owned in handles {
            match self.loop_handle.insert_source(
                calloop::generic::Generic::new(
                    owned,
                    calloop::Interest::READ,
                    calloop::Mode::Level,
                ),
                |_, _, runtime| {
                    runtime.request_x11();
                    Ok(calloop::PostAction::Disable)
                },
            ) {
                Ok(token) => watchers.push(token),
                Err(error) => {
                    for token in watchers {
                        self.loop_handle.remove(token);
                    }
                    return Err(Box::new(error));
                }
            }
        }
        self.x11_watchers = watchers;
        self.x11_sockets = Some(sockets);
        self.set_x11_display(number);
        Ok(())
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

    #[cfg(feature = "xwayland")]
    pub(crate) fn x11_startup_failed(&mut self) {
        self.x11_failed = true;
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
        if self.state_path.is_none() {
            return;
        }
        let keyboard = self.manager.keyboard_summary(&mut self.state);
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
        // Previews only once the transition settles: harnesses click
        // where they say.
        let scene =
            (overview_open && self.overview_progress >= 1.0).then(|| self.overview_layout());
        // Record the drawn transition separately from settled click targets.
        let transition_scene = (self.overview_progress > 0.0).then(|| self.overview_layout());
        let workspace_cards: Vec<serde_json::Value> = transition_scene
            .iter()
            .flat_map(|l| l.cards.iter())
            .map(|c| {
                serde_json::json!({
                    "workspace": c.workspace, "active": c.active, "alpha": c.alpha,
                    "rect": [c.rect.loc.x, c.rect.loc.y, c.rect.size.w, c.rect.size.h],
                })
            })
            .collect();
        // GNOME's workspace thumbnails strip (three or more workspaces).
        let thumbnails: Vec<serde_json::Value> = transition_scene
            .iter()
            .flat_map(|l| l.thumbnails.iter())
            .map(|t| {
                serde_json::json!({
                    "workspace": t.workspace,
                    "active": t.active,
                    "alpha": t.alpha,
                    "rect": [t.rect.loc.x, t.rect.loc.y, t.rect.size.w, t.rect.size.h],
                })
            })
            .collect();
        let previews: Vec<serde_json::Value> = if let Some(scene) = &scene {
            scene
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
        #[cfg(feature = "xwayland")]
        let x11_ready = self.state.xwm.is_some();
        #[cfg(not(feature = "xwayland"))]
        let x11_ready = false;
        let doc = serde_json::json!({
            "x11_display": self.x11_display.map(|d| format!(":{d}")),
            "x11_ready": x11_ready,
            "idle_timeout_ms": self.lock.timeout_ms(),
            "idle_blank_alpha": self.blank_alpha(),
            "overview_search": self.overview_search,
            "overview_app_grid": self.overview_app_grid,
            "keyboard": keyboard,
            "overview_open": overview_open,
            "animations_enabled": self.input_settings.enable_animations,
            "locked": self.is_locked(),
            "active_workspace": model.active_workspace(),
            "focused": focused,
            "focused_app_id": focused.and_then(app_of),
            "focused_rect": focused
                .and_then(|id| self.manager.geometry(id))
                .map(|g| [g.loc.x, g.loc.y, g.size.w, g.size.h]),
            // Floating ("gnome") or niri's strip ("scroll"), and how far
            // the strip's view has scrolled.
            "session_mode": match self.manager.session_mode() {
                crate::windows::SessionMode::Scroll => "scroll",
                crate::windows::SessionMode::Gnome => "gnome",
            },
            "strip_offset": self.manager.strip_offset(),
            // Where the strip is drawn this frame: trails the target
            // on niri's view-movement spring, equal once settled.
            "strip_view": self.manager.strip_view(),
            "minimized": snapshot
                .windows
                .iter()
                .map(|w| w.id)
                .filter(|id| self.manager.is_minimized(*id))
                .collect::<Vec<_>>(),
            "above": snapshot
                .windows
                .iter()
                .map(|w| w.id)
                .filter(|id| self.manager.is_above(*id))
                .collect::<Vec<_>>(),
            "windows": self.manager.overview_windows().iter().map(|w| serde_json::json!({
                "id": w.id,
                "app_id": app_of(w.id),
                "workspace": w.workspace,
                "rect": [w.geometry.loc.x, w.geometry.loc.y, w.geometry.size.w, w.geometry.size.h],
            })).collect::<Vec<_>>(),
            "previews": previews,
            "thumbnails": thumbnails,
            "workspace_placeholder": scene.as_ref().and_then(|s| s.placeholder).map(|(at, r)| serde_json::json!({
                "workspace": at, "rect": [r.loc.x, r.loc.y, r.size.w, r.size.h],
            })),
            "overview_progress": self.overview_progress,
            "workspace_cards": workspace_cards,

            // The Alt+Tab switcher's window thumbnails being drawn.
            "switcher_thumbnails": self.switcher_thumbnails.len(),
            // GNOME's tile preview while a dragged window is over a
            // snap edge: the window and the area it would fill.
            "tile_preview": self.manager.tile_preview(&self.state).map(|(id, r)| {
                serde_json::json!({
                    "window": id,
                    "rect": [r.loc.x, r.loc.y, r.size.w, r.size.h],
                })
            }),
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
        let layout = if self.overview_app_grid {
            crate::overview::app_grid_layout
        } else {
            crate::overview::layout
        };
        let mut scene = layout(
            output,
            crate::windows::WORK_AREA_TOP,
            model.workspaces(),
            model.active_workspace(),
            &self.manager.overview_windows(),
        );
        if self.overview_progress < 1.0 {
            // Part-way through GNOME's transition.
            return crate::overview::transition(
                &scene,
                crate::overview::ease_out_quad(self.overview_progress),
                output,
                &self.manager.overview_windows(),
            );
        }
        match self.overview_drag {
            Some((id, start, true)) => {
                if let Some(at) =
                    crate::overview::insertion_target(&scene, self.manager.pointer_pos())
                {
                    crate::overview::show_placeholder(&mut scene, at);
                }
                crate::overview::drag_preview(&mut scene, id, start, self.manager.pointer_pos());
            }
            // GNOME grows the preview under the pointer by 5px a side.
            _ => crate::overview::grow_hovered(&mut scene, self.manager.pointer_pos()),
        }
        scene
    }

    /// Move the overview transition toward the open state (250 ms each
    /// way, as GNOME's), unless a swipe holds it. Returns whether the
    /// overview is drawn at all.
    fn step_overview_transition(&mut self) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.overview_progress_at).as_secs_f64() * 1000.0;
        self.overview_progress_at = now;
        if self.overview_swipe_from.is_none() {
            let target = if self.control.overview_open() {
                1.0
            } else {
                0.0
            };
            let step = if self.input_settings.enable_animations {
                dt / crate::overview::TRANSITION_MS
            } else {
                1.0
            };
            self.overview_progress = if target > self.overview_progress {
                (self.overview_progress + step).min(target)
            } else {
                (self.overview_progress - step).max(target)
            };
        }
        self.overview_progress > 0.0
    }

    fn animated_tile_preview(
        &mut self,
        target: Option<(u64, Rectangle<i32, Logical>)>,
    ) -> Option<(u64, Rectangle<i32, Logical>)> {
        let Some((id, to)) = target else {
            self.tile_animation = None;
            return None;
        };
        let now = Instant::now();
        let unchanged = self
            .tile_animation
            .as_ref()
            .is_some_and(|(old, animation, _)| *old == id && animation.to == to);
        if !unchanged {
            let from = self
                .tile_animation
                .as_ref()
                .filter(|(old, _, _)| *old == id)
                .map(|(_, animation, start)| {
                    animation.value_at(
                        now.duration_since(*start).as_secs_f64(),
                        self.input_settings.enable_animations,
                    )
                })
                .or_else(|| self.manager.render_geometry(id))
                .unwrap_or(to);
            self.tile_animation = Some((id, crate::animation::EaseRect { from, to }, now));
        }
        let (_, animation, start) = self.tile_animation.as_ref()?;
        Some((
            id,
            animation.value_at(
                now.duration_since(*start).as_secs_f64(),
                self.input_settings.enable_animations,
            ),
        ))
    }

    /// Advance the scroll-mode strip view on its spring by the time
    /// since the last frame (niri's view-movement animation).
    fn step_strip_view(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.strip_view_at).as_secs_f64();
        self.strip_view_at = now;
        self.manager.step_strip_view(dt);
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
                // A click (focus) or a drag (move to a workspace): the
                // release decides.
                self.overview_drag = Some((id, pos, false));
            }
            crate::overview::OverviewHit::Workspace(ws) => {
                // GNOME's trailing empty workspace is not in the model
                // yet: it sits right after the active (last) one, and
                // stepping onto it creates it.
                if !self.manager.switch_to_workspace(&mut self.state, ws) {
                    // From a thumbnail the active one need not be last.
                    if let Some(&last) = self.manager.model().workspaces().last() {
                        self.manager.switch_to_workspace(&mut self.state, last);
                    }
                    self.manager.switch_relative(&mut self.state, 1);
                }
            }
            crate::overview::OverviewHit::Dismiss => self.control.set_overview(false),
        }
        true
    }

    /// The button came up after a press on a preview: a click focuses
    /// the window and closes the overview; a drag dropped on a
    /// thumbnail or a neighboring card moves the window to that
    /// workspace (GNOME), anywhere else puts it back.
    fn overview_release(&mut self) {
        let Some((id, _, dragging)) = self.overview_drag.take() else {
            return;
        };
        if !dragging {
            self.manager.focus(&mut self.state, Some(id));
            self.control.set_overview(false);
            return;
        }
        let pos = self.manager.pointer_pos();
        let layout = self.overview_layout();
        if let Some(at) = crate::overview::insertion_target(&layout, pos) {
            self.manager
                .insert_workspace_and_move(&mut self.state, id, at);
            return;
        }
        let Some(target) = crate::overview::drop_target(&layout, pos) else {
            return;
        };
        let here = self.manager.model().window(id).map(|w| w.workspace);
        if here != Some(target) {
            self.manager.move_to_workspace(&mut self.state, id, target);
        }
    }

    /// Feed explicitly opted-in nested proofs through the real gesture handler.
    /// Each atomically replaced JSON file is consumed once; ordinary sessions
    /// never read it because state instrumentation is also required.
    fn proof_swipe_input(&mut self) {
        if self.state_path.is_none() {
            return;
        }
        let Some(path) = std::env::var_os("ROOST_PROOF_SWIPE_INPUT")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
        else {
            return;
        };
        if !std::fs::metadata(&path).is_ok_and(|m| m.len() <= 4096) {
            return;
        }
        let Ok(body) = std::fs::read_to_string(path) else {
            return;
        };
        if body == self.proof_swipe_last {
            return;
        }
        self.proof_swipe_last = body.clone();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            return;
        };
        let time = crate::state::system_millis() as u32;
        let input = match value["phase"].as_str() {
            Some("begin") => ManagerInput::SwipeBegin { fingers: 3, time },
            Some("update") => ManagerInput::SwipeUpdate {
                delta: (
                    value["dx"].as_f64().unwrap_or(0.0),
                    value["dy"].as_f64().unwrap_or(0.0),
                )
                    .into(),
                time,
            },
            Some("end") => ManagerInput::SwipeEnd {
                cancelled: value["cancelled"].as_bool().unwrap_or(false),
                time,
            },
            _ => return,
        };
        self.on_manager_input(input);
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
                    let travel = *travel;
                    // GNOME's overview follows the fingers: a vertical
                    // swipe moves the transition (up opens) as it goes.
                    if travel.y.abs() > travel.x.abs() {
                        let from = *self
                            .overview_swipe_from
                            .get_or_insert(self.overview_progress);
                        let progress = (from - travel.y / OVERVIEW_SWIPE_DISTANCE).clamp(0.0, 1.0);
                        self.overview_progress = if self.input_settings.enable_animations {
                            progress
                        } else if progress >= 0.5 {
                            1.0
                        } else {
                            0.0
                        };
                    }
                    true
                }
                None => false,
            },
            ManagerInput::SwipeEnd { cancelled, .. } => {
                let Some(travel) = self.shell_swipe.take() else {
                    return false;
                };
                if let Some(from) = self.overview_swipe_from.take() {
                    // Released: finish toward whichever side is nearer
                    // (back where it began when cancelled).
                    let open = if cancelled {
                        from >= 0.5
                    } else {
                        self.overview_progress >= 0.5
                    };
                    self.control.set_overview(open);
                    return true;
                }
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

    /// Save the focused window alone as a PNG (GNOME's
    /// `ScreenshotWindow`): its visible geometry, popups included,
    /// client shadows left out. Never while locked.
    pub fn capture_window(&mut self, requested: &std::path::Path) -> Option<std::path::PathBuf> {
        if self.is_locked() {
            return None;
        }
        let id = self.manager.model().focused()?;
        let path =
            crate::screenshot::target_path(requested, &crate::screenshot::jiff_like::Stamp::now())?;
        let (w, h, rgba) =
            self.render_window_pixels(id, None, smithay::backend::allocator::Fourcc::Abgr8888)?;
        crate::screenshot::save_png(&path, w as u32, h as u32, &rgba).ok()?;
        eprintln!(
            "roost-compositor: window screenshot saved to {}",
            path.display()
        );
        Some(path)
    }

    /// GNOME's screenshot window selector (screenshot.js
    /// `UIWindowSelector.capture`): every window on the active workspace,
    /// minimized ones left out, each saved at full size into `directory`
    /// and given its slot in the selector, laid out with GNOME's window
    /// spread inside the selector's margins. Never while locked.
    pub fn capture_selector_windows(
        &mut self,
        directory: &std::path::Path,
    ) -> Vec<crate::screenshot::WindowShot> {
        if self.is_locked() || std::fs::create_dir_all(directory).is_err() {
            return Vec::new();
        }
        let size = self.state.primary_size();
        let model = self.manager.model();
        let active = model.active_workspace();
        let focused = model.focused();
        let mut windows: Vec<(u64, Rectangle<i32, Logical>)> = self
            .manager
            .overview_windows()
            .into_iter()
            .filter(|w| w.workspace == active && !self.manager.is_minimized(w.id))
            .map(|w| (w.id, w.geometry))
            .collect();
        // GNOME sorts by stable sequence: creation order (Roost ids rise).
        windows.sort_by_key(|(id, _)| *id);
        let top = crate::windows::WORK_AREA_TOP;
        let workarea = Rectangle::new((0, top).into(), (size.w, (size.h - top).max(1)).into());
        let (ax, ay, aw, ah) = crate::screenshot::selector_area(size.w, size.h);
        let area = Rectangle::new((ax, ay).into(), (aw, ah).into());
        let slots = crate::overview::window_slots_spaced(
            workarea,
            size.h,
            area,
            &windows,
            crate::overview::SELECTOR_SPACING,
        );
        let mut shots = Vec::new();
        for (id, rect, _) in slots {
            let title = self
                .manager
                .model()
                .window(id)
                .map(|w| w.title.clone())
                .unwrap_or_default();
            let Some((w, h, rgba)) =
                self.render_window_pixels(id, None, smithay::backend::allocator::Fourcc::Abgr8888)
            else {
                continue;
            };
            let path = directory.join(format!("window-{id}.png"));
            if crate::screenshot::save_png(&path, w as u32, h as u32, &rgba).is_err() {
                continue;
            }
            shots.push(crate::screenshot::WindowShot {
                id,
                title,
                focused: focused == Some(id),
                x: rect.loc.x,
                y: rect.loc.y,
                width: rect.size.w,
                height: rect.size.h,
                path,
            });
        }
        shots
    }

    /// Physical size of window `id` at its output's scale, and the scale.
    fn window_pixel_size(&self, id: u64) -> Option<((i32, i32), f64)> {
        let geometry = self.manager.geometry(id)?;
        let scale = match &self.backend {
            Backend::Winit(_) => self.scale,
            #[cfg(feature = "drm")]
            Backend::Drm(_) => self.state.scale_for(geometry).fractional_scale(),
        };
        let px = |v: i32| ((f64::from(v) * scale).round() as i32).max(1);
        Some(((px(geometry.size.w), px(geometry.size.h)), scale))
    }

    /// Render one window offscreen with its visible geometry at (0, 0),
    /// into `size` physical pixels (default: the window's own), and read
    /// it back in `fourcc` byte order. Never while locked.
    fn render_window_pixels(
        &mut self,
        id: u64,
        size: Option<(i32, i32)>,
        fourcc: smithay::backend::allocator::Fourcc,
    ) -> Option<(i32, i32, Vec<u8>)> {
        use smithay::backend::renderer::{ExportMem, Offscreen};
        if self.is_locked() {
            return None;
        }
        let surface = self.manager.surface_of(id)?;
        let (natural, scale) = self.window_pixel_size(id)?;
        let (w, h) = size.unwrap_or(natural);
        let view = View {
            offset: (0, 0),
            scale,
        };
        let renderer = match &mut self.backend {
            Backend::Winit(backend) => backend.renderer(),
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => &mut drm.renderer,
        };
        // Surface-local origin such that the window geometry (inside any
        // client-side shadow) starts at the buffer's corner.
        let geo = crate::popup::window_geometry_loc(&surface);
        let origin: Point<i32, Logical> = (-geo.x, -geo.y).into();
        let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
        let mut tree =
            |renderer: &mut GlesRenderer,
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
        tree(renderer, &surface, origin);
        for popup in crate::popup::placed_popups(&surface, origin, true) {
            tree(renderer, &popup.surface, popup.origin);
        }
        let elements = Scene::flat(crate::layer::front_to_back(elements));
        let buffer_size = (w, h).into();
        let mut texture: smithay::backend::renderer::gles::GlesTexture =
            renderer.create_buffer(fourcc, buffer_size).ok()?;
        let size: smithay::utils::Size<i32, smithay::utils::Physical> = (w, h).into();
        let mut target = renderer.bind(&mut texture).ok()?;
        {
            let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
            draw_scene(
                &mut frame,
                Color32F::new(0.0, 0.0, 0.0, 0.0),
                &[],
                &elements,
                Target {
                    damage: Rectangle::from_size(size),
                    scale,
                    blank_alpha: 0.0,
                },
                &[],
                &[],
            )
            .ok()?;
            let _ = frame.finish().ok()?;
        }
        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size(buffer_size), fourcc)
            .ok()?;
        let bytes = renderer.map_texture(&mapping).ok()?.to_vec();
        Some((w, h, bytes))
    }

    /// The desktop clear: GNOME's `primary-color` once the shell has
    /// published it, else a dark neutral.
    fn desktop_color(&self) -> Color32F {
        match self.wallpaper.color() {
            Some([r, g, b]) => Color32F::new(r, g, b, 1.0),
            None => Color32F::new(0.08, 0.09, 0.11, 1.0),
        }
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
        let blank_alpha = self.blank_alpha();
        let overview = (self.overview_progress > 0.0).then(|| self.overview_layout());
        let cards = overview.as_ref().filter(|_| !self.overview_search);
        let tile = overview
            .is_none()
            .then(|| self.manager.tile_preview(&self.state))
            .flatten();
        let tile = self.animated_tile_preview(tile);
        let accent = self.wallpaper.accent();
        let decor_global = cards
            .map(|layout| overview_decor(layout, accent))
            .unwrap_or_default();
        let background = if overview.is_some() {
            OVERVIEW_BACKGROUND
        } else {
            self.desktop_color()
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
        let mut elements = scene_elements(
            renderer,
            &self.manager,
            &self.state,
            view,
            true,
            overview.as_ref(),
            None,
            tile.as_ref()
                .and_then(|(id, _)| self.manager.surface_of(*id))
                .as_ref(),
        );
        if let Some((_, rect)) = tile {
            elements.tile = tile_elements(rect, view, accent);
        }
        if overview.is_none() {
            elements
                .tile
                .extend(focus_ring_elements(&self.manager, view));
        }
        elements.top = previews_to_elements(
            renderer,
            &self.manager,
            view,
            &switcher_previews(&self.manager, &self.switcher_thumbnails),
        );
        elements.top.extend(dnd_icon_elements(
            renderer,
            &self.state,
            view,
            self.manager.pointer_pos(),
        ));
        let decor = decor_for_output(&decor_global, view);
        let previews = preview_elements(renderer, &self.manager, view, cards);
        let paper = backdrop(
            &mut self.wallpaper,
            renderer,
            size,
            view,
            overview.is_none(),
            cards,
            false,
        );
        let damage = Rectangle::from_size(size);
        let mut target = renderer.bind(&mut texture).ok()?;
        {
            let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
            draw_scene(
                &mut frame,
                background,
                &paper,
                &elements,
                Target {
                    damage,
                    scale: view.scale,
                    blank_alpha,
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
        use crate::mutter::CastTarget;
        // A cast window that closed ends its session, as in Mutter.
        let manager = &self.manager;
        self.casts.retain(|cast| {
            let gone =
                matches!(cast.target, CastTarget::Window(id) if manager.geometry(id).is_none());
            if gone {
                cast.close();
            }
            !cast.failed() && !gone
        });
        // Window casts follow their window's size.
        for index in 0..self.casts.len() {
            if let CastTarget::Window(id) = self.casts[index].target {
                if let Some(((w, h), _)) = self.window_pixel_size(id) {
                    self.casts[index].resize(w, h);
                }
            }
        }
        let wanted: Vec<(usize, CastTarget, (i32, i32))> = self
            .casts
            .iter()
            .enumerate()
            .filter_map(|(i, cast)| {
                cast.wants_frame()
                    .map(|size| (i, cast.target.clone(), size))
            })
            .collect();
        let xrgb = smithay::backend::allocator::Fourcc::Xrgb8888;
        for (index, target, size) in wanted {
            let frame = match &target {
                CastTarget::Window(id) => self.render_window_pixels(*id, Some(size), xrgb),
                CastTarget::Monitor(connector) => {
                    let output = match &self.backend {
                        Backend::Winit(_) => None,
                        #[cfg(feature = "drm")]
                        Backend::Drm(_) => Some(connector.as_str()),
                    };
                    self.render_pixels(output, xrgb)
                }
                CastTarget::Area(connector, area) => {
                    let output = match &self.backend {
                        Backend::Winit(_) => None,
                        #[cfg(feature = "drm")]
                        Backend::Drm(_) => Some(connector.as_str()),
                    };
                    #[cfg(not(feature = "drm"))]
                    let _ = connector;
                    self.render_pixels(output, xrgb).and_then(|(w, h, pixels)| {
                        let cropped = crate::screencast::crop(&pixels, (w, h), *area);
                        if cropped.is_none() {
                            eprintln!(
                                "roost-compositor: screen cast area {area:?} is not inside the {w}x{h} frame ({} bytes)",
                                pixels.len()
                            );
                        }
                        cropped.map(|cropped| (area.2, area.3, cropped))
                    })
                }
            };
            let Some((w, h, bgrx)) = frame else {
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
                target,
                signal,
            } => {
                if self.pipewire.is_none() {
                    self.pipewire = crate::screencast::PipeWire::new(&self.loop_handle);
                    if self.pipewire.is_none() {
                        eprintln!("roost-compositor: screen cast: PipeWire is not running");
                        crate::mutter::session_closed(&signal, session_id);
                        return;
                    }
                }
                let size = match &target {
                    crate::mutter::CastTarget::Monitor(connector) => self
                        .cast_outputs
                        .lock()
                        .ok()
                        .and_then(|o| o.iter().find(|o| &o.connector == connector).cloned())
                        .map(|o| (o.width, o.height)),
                    crate::mutter::CastTarget::Window(id) => {
                        self.window_pixel_size(*id).map(|(size, _)| size)
                    }
                    crate::mutter::CastTarget::Area(_, (_, _, w, h)) => Some((*w, *h)),
                };
                let Some((width, height)) = size else {
                    crate::mutter::session_closed(&signal, session_id);
                    return;
                };
                let Some(pw) = self.pipewire.as_ref() else {
                    return;
                };
                match pw.start_cast(session_id, target, width, height, signal) {
                    Some(cast) => self.casts.push(cast),
                    None => eprintln!("roost-compositor: screen cast: stream failed to start"),
                }
            }
            crate::mutter::ToLoop::StopCast { session_id } => {
                self.casts.retain(|cast| cast.session_id != session_id);
            }
            crate::mutter::ToLoop::ApplyMonitors {
                configs,
                persistent,
            } => {
                self.apply_monitors(&configs);
                if persistent {
                    match crate::monitors::save(&configs) {
                        Ok(path) => eprintln!(
                            "roost-compositor: display settings saved to {}",
                            path.display()
                        ),
                        Err(e) => eprintln!("roost-compositor: display settings not saved: {e}"),
                    }
                }
            }
        }
    }

    /// Apply GNOME's input settings (#60): the seat's keymap and key
    /// repeat, libinput pointer devices (hardware), the hot corner.
    fn apply_input_settings(&mut self, settings: roost_shell_control::InputSettings) {
        self.manager
            .apply_keyboard_settings(&mut self.state, &settings);
        self.triggers.set_hot_corner(settings.hot_corners);
        self.manager
            .set_animations_enabled(settings.enable_animations);
        if !settings.enable_animations {
            self.overview_progress = if self.control.overview_open() {
                1.0
            } else {
                0.0
            };
        }
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &mut self.backend {
            drm.apply_input_settings(&settings);
        }
        self.input_settings = settings;
    }

    /// Apply a display arrangement live (GNOME Settings' Displays
    /// panel): each named output's scale and logical position. Layout,
    /// rendering and the per-window client scale follow on the next frame.
    fn apply_monitors(&mut self, configs: &[crate::monitors::MonitorConfig]) {
        let scale_of = |s: f64| {
            if s == 1.0 {
                Scale::Integer(1)
            } else {
                Scale::Fractional(s)
            }
        };
        match &mut self.backend {
            Backend::Winit(backend) => {
                // One nested output: take the first arrangement entry.
                let Some(config) = configs.first() else {
                    return;
                };
                self.scale = clamp_scale(config.scale);
                if let Some(output) = self.state.primary_output() {
                    output.change_current_state(None, None, Some(scale_of(self.scale)), None);
                }
                let size = backend.window_size();
                let (lw, lh) = logical_size(size.w, size.h, self.scale);
                self.state.set_output_size(lw, lh);
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                for config in configs {
                    let Some(out) = drm.outputs.iter_mut().find(|o| o.name == config.connector)
                    else {
                        continue;
                    };
                    out.scale = clamp_scale(config.scale);
                    out.loc = (config.x, config.y);
                    out.output.change_current_state(
                        None,
                        None,
                        Some(scale_of(out.scale)),
                        Some(out.loc.into()),
                    );
                    let (lw, lh) = out.logical_size();
                    self.state
                        .add_output(&out.name, Some(out.output.clone()), lw, lh);
                    self.state.set_output_location(&out.name, out.loc);
                }
            }
        }
        if let Some(first) = configs.first() {
            self.state.set_preferred_scale(clamp_scale(first.scale));
        }
    }

    /// Keep the D-Bus side's monitor list current (cheap when unchanged).
    /// Preview geometry for the shell's preview chrome: GNOME's app
    /// icon, caption and close button (window picker only, not while
    /// search or the app grid covers it).
    fn publish_overview_previews(&mut self) {
        let (previews, hovered) = if self.control.overview_open()
            && self.overview_progress >= 1.0
            && !self.overview_search
            && !self.overview_app_grid
        {
            let scene = self.overview_layout();
            let list: Vec<roost_shell_control::PreviewInfo> = scene
                .previews
                .iter()
                .filter(|p| p.active)
                .map(|p| roost_shell_control::PreviewInfo {
                    window: p.id,
                    x: p.rect.loc.x,
                    y: p.rect.loc.y,
                    width: p.rect.size.w,
                    height: p.rect.size.h,
                })
                .collect();
            (list, scene.hovered)
        } else {
            (Vec::new(), None)
        };
        self.control.set_overview_previews(previews, hovered);
    }

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
        // Windows for Introspect (signals only on change).
        let model = self.manager.model();
        let active = model.active_workspace();
        let windows = model
            .windows()
            .filter_map(|entry| {
                let geometry = self.manager.geometry(entry.id)?;
                Some(crate::mutter::WindowSnapshot {
                    id: entry.id,
                    title: entry.title.clone(),
                    app_id: entry.app_id.clone(),
                    width: geometry.size.w,
                    height: geometry.size.h,
                    focused: entry.focused,
                    hidden: entry.workspace != active,
                    x11: self.manager.is_x11(entry.id),
                })
            })
            .collect();
        self.introspect.publish(windows);
    }

    fn tick(&mut self) -> Result<bool, RuntimeError> {
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            let text =
                std::fs::read_to_string(std::path::PathBuf::from(dir).join("roost-idle-blank"))
                    .unwrap_or_default();
            self.blank = crate::lock::IdleBlank::parse(&text);
        }
        self.display
            .dispatch_clients(&mut self.state)
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.display
            .flush_clients()
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
        self.manager.reconcile(&mut self.state);
        // A client's pointer warp moved the manager's pointer: the
        // drawn cursor follows (#89).
        #[cfg(feature = "drm")]
        if let Backend::Drm(drm) = &mut self.backend {
            drm.set_pointer(self.manager.pointer_pos());
        }
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
        if let Some(active) = outcome.overview_app_grid {
            self.overview_app_grid = active;
        }
        if let Some(settings) = outcome.input_settings {
            self.apply_input_settings(settings);
        }
        if let Some(list) = outcome.accelerators {
            self.manager.set_accelerators(list);
        }
        for (window, action) in outcome.window_actions {
            self.manager.window_action(&mut self.state, window, action);
        }
        if let Some(thumbnails) = outcome.switcher_thumbnails {
            self.switcher_thumbnails = thumbnails;
        }
        if let Some(keys) = outcome.switcher_keys {
            self.manager.set_switcher_keys(keys);
        }
        for backward in outcome.input_source_switches {
            self.manager.switch_input_source(&mut self.state, backward);
        }
        // Header-bar right clicks: GNOME's window menu, drawn by the shell.
        for (window, x, y) in self.manager.take_menu_requests() {
            self.control
                .queue_message(roost_shell_control::Message::WindowMenu {
                    window,
                    x,
                    y,
                    maximized: self.manager.is_maximized(window),
                    above: self.manager.is_above(window),
                    sticky: self.manager.is_sticky(window),
                    workspace_left: self.manager.workspace_left_of(window),
                    workspace_right: true,
                });
        }
        if !self.control.overview_open() {
            self.overview_search = false;
            self.overview_app_grid = false;
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
        // ext-session-lock-v1: only the supervised shell may lock, and
        // locking through the protocol engages the compositor's own
        // flag. Anyone else's locker is dropped, which tells it the
        // lock failed.
        if let Some((locker, pid)) = self.state.take_lock_request() {
            let ours = matches!(
                (pid, self.shell.child_pid()),
                (Some(pid), Some(child)) if u32::try_from(pid).ok() == Some(child)
            );
            if ours {
                if !self.is_locked() {
                    self.engage_lock();
                }
            } else {
                eprintln!("roost-compositor: session lock refused for pid {pid:?}");
            }
            self.state.resolve_lock_request(locker, ours);
        }
        // A lock client's unlock_and_destroy never unlocks by itself:
        // only a verified password clears the flag.
        if self.state.take_client_unlock() && self.is_locked() {
            eprintln!("roost-compositor: lock client left while locked; staying locked");
        }
        // A lock-screen password: verified off the loop (PAM may stall
        // for seconds on a failure), one attempt at a time.
        if let Some((request, password)) = outcome.unlock {
            if self.unlock_inflight || !self.is_locked() {
                let unlocked = !self.is_locked();
                self.control.finish_unlock(request, unlocked);
            } else {
                self.unlock_inflight = true;
                if std::env::var_os("ROOST_LOCK_TRACE").is_some() {
                    eprintln!("roost-compositor: lock authentication started");
                }
                let reply = self.unlock_results.clone();
                std::thread::spawn(move || {
                    let ok = crate::unlock::session_user()
                        .is_some_and(|user| crate::unlock::verify(&user, &password.0));
                    let _ = reply.send((request, ok));
                });
            }
        }
        self.idle_monitor.tick();
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
            if let Some(ime) = &mut self.ime {
                ime.poll(crate::state::system_millis(), &mut self.display.handle());
            }
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
            let now = crate::state::system_millis();
            if self.x11_failed
                || self
                    .active_x11_client
                    .as_ref()
                    .is_some_and(|client| client.get_credentials(&display).is_err())
            {
                if let Some(token) = self.x11_source.take() {
                    loop_handle.remove(token);
                }
                self.active_x11_client = None;
                self.x11_failed = false;
                self.pending_x11_client = None;
                self.state.xwm = None;
                self.manager.clear_x11_windows(&mut self.state);
                self.xwayland.disconnected(now);
            }
            let pending = &mut self.pending_x11_client;
            let reserved = &mut self.x11_sockets;
            let watchers = &mut self.x11_watchers;
            let source = &mut self.x11_source;
            let client = &mut self.active_x11_client;
            let advertised = self.x11_display;
            let mut spawner = || {
                for token in watchers.drain(..) {
                    loop_handle.remove(token);
                }
                let sockets = match reserved.take() {
                    Some(sockets) => sockets,
                    None => smithay::xwayland::XWaylandSockets::prepare(advertised, true)
                        .map_err(|_| crate::xwayland::AbsentReason::SpawnFailed)?,
                };
                crate::xwayland::spawn_xwayland(
                    &display,
                    &loop_handle,
                    pending,
                    sockets,
                    source,
                    client,
                )
            };
            self.xwayland.tick(now, Some(&mut spawner));
        }
        self.stats.shell_restarts = self.shell.restarts_used();
        self.proof_swipe_input();
        self.publish_state();
        self.publish_cast_outputs();
        self.publish_overview_previews();
        self.render()?;
        self.screencast_tick();
        Ok(!self.exit)
    }

    /// A page flip completed (hardware): mark what the frame drew
    /// presented with the kernel's vblank time and sequence, and on the
    /// primary output advance fifo barriers and commit timers (#89).
    #[cfg(feature = "drm")]
    fn page_flipped(&mut self, flip: crate::drm::PageFlip) {
        use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
        use smithay::utils::{Monotonic, Time};
        // Mutter's KMS flags: vsync'd, kernel-timestamped, completion
        // reported by the hardware.
        let (time, flags): (Time<Monotonic>, Kind) = match flip.time {
            Some(time) => (
                Time::from(time),
                Kind::Vsync | Kind::HwClock | Kind::HwCompletion,
            ),
            None => (
                self.state.presentation_now(),
                Kind::Vsync | Kind::HwCompletion,
            ),
        };
        if let Some(mut feedback) = flip.feedback {
            feedback.presented::<_, Monotonic>(
                time,
                smithay::wayland::presentation::Refresh::fixed(flip.refresh),
                flip.sequence,
                flags,
            );
        }
        if flip.primary {
            let roots = crate::frame_timing::frame_roots(&self.state, &self.manager);
            self.state.refresh_cycle(&roots, time, flip.refresh);
        }
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
        let blank_alpha = self.blank_alpha();
        let show_content = content_visible(locked);
        let overlay_visible = self.overlay.visible;
        let background = if locked {
            // GNOME's lock screen dims the desktop to 65%; without a
            // picture that is the dimmed primary-color.
            let c = self.desktop_color();
            let k = roost_wallpaper::LOCK_BRIGHTNESS;
            Color32F::new(c.r() * k, c.g() * k, c.b() * k, 1.0)
        } else if overlay_visible {
            Color32F::new(0.20, 0.08, 0.10, 1.0)
        } else {
            self.desktop_color()
        };
        let drawn = self.step_overview_transition();
        self.step_strip_view();
        let overview = (show_content && !overlay_visible && drawn).then(|| self.overview_layout());
        // While search shows results, the workspace view steps aside.
        let cards = overview.as_ref().filter(|_| !self.overview_search);
        // GNOME's tile preview while a dragged window is over a snap edge.
        let tile = (show_content && overview.is_none())
            .then(|| self.manager.tile_preview(&self.state))
            .flatten();
        let tile = self.animated_tile_preview(tile);
        let accent = self.wallpaper.accent();
        let decor_global = cards
            .map(|layout| overview_decor(layout, accent))
            .unwrap_or_default();
        let background = if overview.is_some() {
            OVERVIEW_BACKGROUND
        } else {
            background
        };
        let show_paper = show_content && !overlay_visible && overview.is_none();
        match &mut self.backend {
            Backend::Winit(backend) => {
                backend.window().set_cursor_visible(blank_alpha < 1.0);
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
                    let lock_surface = locked
                        .then(|| self.state.primary_output_name())
                        .flatten()
                        .and_then(|name| self.state.lock_surface_for(&name));
                    let mut elements = scene_elements(
                        renderer,
                        &self.manager,
                        &self.state,
                        view,
                        show_content,
                        overview.as_ref(),
                        lock_surface.as_ref(),
                        tile.as_ref()
                            .and_then(|(id, _)| self.manager.surface_of(*id))
                            .as_ref(),
                    );
                    if let Some((_, rect)) = tile {
                        elements.tile = tile_elements(rect, view, accent);
                    }
                    if overview.is_none() && show_content {
                        elements
                            .tile
                            .extend(focus_ring_elements(&self.manager, view));
                    }
                    elements.top = previews_to_elements(
                        renderer,
                        &self.manager,
                        view,
                        &switcher_previews(&self.manager, &self.switcher_thumbnails),
                    );
                    elements.top.extend(dnd_icon_elements(
                        renderer,
                        &self.state,
                        view,
                        self.manager.pointer_pos(),
                    ));
                    let decor = decor_for_output(&decor_global, view);
                    let previews = preview_elements(renderer, &self.manager, view, cards);
                    let paper = backdrop(
                        &mut self.wallpaper,
                        renderer,
                        size,
                        view,
                        show_paper,
                        cards,
                        locked,
                    );
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
                        &paper,
                        &elements,
                        Target {
                            damage,
                            scale: view.scale,
                            blank_alpha,
                        },
                        &decor,
                        &previews,
                    )?;
                    let _ = frame
                        .finish()
                        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                }
                send_surface_scales(&self.state, &self.manager);
                send_frame_callbacks(&self.state, &self.manager);
                backend
                    .submit(Some(&[damage]))
                    .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
                // The host took the frame: presentation feedback,
                // fifo barriers and commit timers (#89).
                crate::frame_timing::present_nested_frame(
                    &mut self.state,
                    &self.manager,
                    locked,
                    self.stats.frames,
                );
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
                // What each output's frame draws, for presentation
                // feedback at its page flip (#89): a surface belongs to
                // the output under its center, else the primary.
                let drawn = crate::frame_timing::drawn_roots(&self.state, &self.manager, locked);
                let rects: Vec<Rectangle<i32, Logical>> = outputs
                    .iter()
                    .map(|o| Rectangle::new(o.loc.into(), o.logical_size().into()))
                    .collect();
                let owner = |at: &smithay::utils::Point<i32, Logical>| {
                    rects.iter().position(|r| r.contains(*at)).unwrap_or(0)
                };
                let mut queued = false;
                for (index, out) in outputs.iter_mut().enumerate() {
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
                    let lock_surface = locked
                        .then(|| self.state.lock_surface_for(&out.name))
                        .flatten();
                    let mut elements = scene_elements(
                        renderer,
                        &self.manager,
                        &self.state,
                        view,
                        show_content,
                        overview.as_ref(),
                        lock_surface.as_ref(),
                        tile.as_ref()
                            .and_then(|(id, _)| self.manager.surface_of(*id))
                            .as_ref(),
                    );
                    if let Some((_, rect)) = tile {
                        elements.tile = tile_elements(rect, view, accent);
                    }
                    if overview.is_none() && show_content {
                        elements
                            .tile
                            .extend(focus_ring_elements(&self.manager, view));
                    }
                    elements.top = previews_to_elements(
                        renderer,
                        &self.manager,
                        view,
                        &switcher_previews(&self.manager, &self.switcher_thumbnails),
                    );
                    elements.top.extend(dnd_icon_elements(
                        renderer,
                        &self.state,
                        view,
                        self.manager.pointer_pos(),
                    ));
                    let decor = decor_for_output(&decor_global, view);
                    let previews = preview_elements(renderer, &self.manager, view, cards);
                    let paper = backdrop(
                        &mut self.wallpaper,
                        renderer,
                        size,
                        view,
                        show_paper,
                        cards,
                        locked,
                    );
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
                            &paper,
                            &elements,
                            Target {
                                damage,
                                scale: view.scale,
                                blank_alpha,
                            },
                            &decor,
                            &previews,
                        )?;
                        // Software pointer on top (no host cursor on
                        // bare hardware); hidden while locked.
                        if !locked && blank_alpha < 1.0 {
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
                    let mine: Vec<_> = drawn
                        .iter()
                        .filter(|(_, at)| owner(at) == index)
                        .map(|(surface, _)| surface.clone())
                        .chain(lock_surface.clone())
                        .collect();
                    let feedback = crate::frame_timing::take_feedback(&mine, &out.output);
                    if let Err(e) = out.surface.queue_buffer(Some(sync), None, feedback) {
                        eprintln!("roost-compositor: drm: queue_buffer {}: {e}", out.name);
                        continue;
                    }
                    out.pending = true;
                    queued = true;
                }
                send_surface_scales(&self.state, &self.manager);
                if queued {
                    // A pending page flip is not another rendered frame.
                    // Granting callbacks on every client dispatch here would
                    // let redraws outrun the display and keep the loop busy.
                    send_frame_callbacks(&self.state, &self.manager);
                    self.stats.frames += 1;
                }
            }
        }
        Ok(())
    }
}

/// Overview backdrop (GNOME 51's `#overviewGroup`, #222226).
/// Vertical finger travel for a whole overview transition (logical
/// touchpad units, as libinput reports swipe deltas).
const OVERVIEW_SWIPE_DISTANCE: f64 = 300.0;

const OVERVIEW_BACKGROUND: Color32F = Color32F::new(34.0 / 255.0, 34.0 / 255.0, 38.0 / 255.0, 1.0);

/// The overview's solid fills (the workspace thumbnails strip) in
/// global logical space.
fn overview_decor(
    layout: &crate::overview::OverviewLayout,
    accent: [f32; 3],
) -> Vec<(Color32F, Vec<Rectangle<i32, Logical>>)> {
    crate::overview::thumbnail_decor(layout, accent)
        .into_iter()
        .map(|([r, g, b, a], rects)| (Color32F::new(r, g, b, a), rects))
        .collect()
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
    previews_to_elements(renderer, manager, view, &layout.previews)
}

/// Window surfaces drawn scaled into preview rects, front to back.
fn previews_to_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    view: View,
    previews: &[crate::overview::Preview],
) -> Vec<PreviewElement> {
    let mut out = Vec::new();
    for preview in previews {
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
            preview.alpha,
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

/// A drag's icon at the pointer, centred on it (front to back).
fn dnd_icon_elements(
    renderer: &mut GlesRenderer,
    state: &State,
    view: View,
    pointer: Point<f64, Logical>,
) -> Vec<PreviewElement> {
    let Some(icon) = state.dnd_icon() else {
        return Vec::new();
    };
    let size = smithay::wayland::compositor::with_states(&icon, |states| {
        states
            .data_map
            .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
            .and_then(|d| d.lock().ok().and_then(|d| d.surface_size()))
    })
    .unwrap_or_default();
    let origin = view.physical(
        pointer.x - f64::from(size.w) / 2.0,
        pointer.y - f64::from(size.h) / 2.0,
    );
    let elements = render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
        renderer,
        &icon,
        origin,
        view.scale,
        1.0,
        Kind::Unspecified,
    );
    crate::layer::front_to_back(
        elements
            .into_iter()
            .map(|e| {
                smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                    e, origin, 1.0,
                )
            })
            .collect(),
    )
}

/// GNOME's switcher thumbnails (altTab.js `_createWindowClone`): each
/// window scaled down (never up) to fit its frame, centred in it.
fn switcher_previews(
    manager: &WindowManager,
    thumbnails: &[roost_shell_control::SwitcherThumbnail],
) -> Vec<crate::overview::Preview> {
    thumbnails
        .iter()
        .filter_map(|t| {
            let geo = manager.geometry(t.window)?;
            let (w, h) = (f64::from(geo.size.w.max(1)), f64::from(geo.size.h.max(1)));
            let scale = (f64::from(t.width) / w)
                .min(f64::from(t.height) / h)
                .min(1.0);
            let (sw, sh) = ((w * scale).round() as i32, (h * scale).round() as i32);
            Some(crate::overview::Preview {
                id: t.window,
                rect: Rectangle::new(
                    (t.x + (t.width - sw) / 2, t.y + (t.height - sh) / 2).into(),
                    (sw, sh).into(),
                ),
                scale,
                active: false,
                alpha: 1.0,
            })
        })
        .collect()
}

/// One output's surfaces, front to back, with GNOME's tile preview
/// slotted in: `elements[..above]` draw over the preview (the dragged
/// window, its popups and the shell's layers), the rest beneath it.
struct Scene {
    elements: Vec<PreviewElement>,
    above: usize,
    tile: Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement>,
    /// Window thumbnails over everything (the Alt+Tab switcher's).
    top: Vec<PreviewElement>,
}

impl Scene {
    /// Surfaces with nothing between them.
    fn flat(elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>) -> Self {
        let above = elements.len();
        Self {
            elements: elements
                .into_iter()
                .map(|element| {
                    use smithay::backend::renderer::element::Element;
                    let origin = element.geometry(1.0.into()).loc;
                    smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                        element, origin, 1.0,
                    )
                })
                .collect(),
            above,
            tile: Vec::new(),
            top: Vec::new(),
        }
    }
}

/// GNOME's tile preview (`.tile-preview`: the accent at half opacity
/// with a 1px accent border) over `rect`, in one output's pixels.
fn tile_elements(
    rect: Rectangle<i32, Logical>,
    view: View,
    accent: [f32; 3],
) -> Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement> {
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::backend::renderer::element::Id;
    let top_left = view.physical(f64::from(rect.loc.x), f64::from(rect.loc.y));
    let bottom_right = view.physical(
        f64::from(rect.loc.x + rect.size.w),
        f64::from(rect.loc.y + rect.size.h),
    );
    let r = Rectangle::from_extremities(top_left, bottom_right);
    let b = (view.scale.round() as i32).max(1);
    let (w, h) = (r.size.w, r.size.h);
    if w <= 2 * b || h <= 2 * b {
        return Vec::new();
    }
    let [cr, cg, cb] = accent;
    let border = Color32F::new(cr, cg, cb, 1.0);
    // Premultiplied: half opacity halves every channel.
    let fill = Color32F::new(cr * 0.5, cg * 0.5, cb * 0.5, 0.5);
    let at = |x: i32, y: i32, w: i32, h: i32| -> Rectangle<i32, smithay::utils::Physical> {
        Rectangle::new((r.loc.x + x, r.loc.y + y).into(), (w, h).into())
    };
    // Stable ids keep damage tracking quiet while the preview holds.
    thread_local! {
        static IDS: [Id; 5] = std::array::from_fn(|_| Id::new());
    }
    IDS.with(|ids| {
        [
            (at(0, 0, w, b), border),
            (at(0, h - b, w, b), border),
            (at(0, b, b, h - 2 * b), border),
            (at(w - b, b, b, h - 2 * b), border),
            (at(b, b, w - 2 * b, h - 2 * b), fill),
        ]
        .into_iter()
        .zip(ids.iter())
        .map(|((geo, color), id)| {
            SolidColorRenderElement::new(id.clone(), geo, 0usize, color, Kind::Unspecified)
        })
        .collect()
    })
}

/// niri's focus ring in scroll mode (its default `focus-ring`): 4px of
/// #7fc8ff around the focused column, outside its edges, where the
/// strip's 16px gaps leave room.
fn focus_ring_elements(
    manager: &WindowManager,
    view: View,
) -> Vec<smithay::backend::renderer::element::solid::SolidColorRenderElement> {
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::backend::renderer::element::Id;
    const WIDTH: i32 = 4;
    if manager.session_mode() != crate::windows::SessionMode::Scroll {
        return Vec::new();
    }
    let Some(rect) = manager
        .model()
        .focused()
        .and_then(|id| manager.render_geometry(id))
    else {
        return Vec::new();
    };
    let outer = view.physical(f64::from(rect.loc.x - WIDTH), f64::from(rect.loc.y - WIDTH));
    let outer_end = view.physical(
        f64::from(rect.loc.x + rect.size.w + WIDTH),
        f64::from(rect.loc.y + rect.size.h + WIDTH),
    );
    let r = Rectangle::from_extremities(outer, outer_end);
    let b = (f64::from(WIDTH) * view.scale).round().max(1.0) as i32;
    let (w, h) = (r.size.w, r.size.h);
    if w <= 2 * b || h <= 2 * b {
        return Vec::new();
    }
    let color = Color32F::new(0x7f as f32 / 255.0, 0xc8 as f32 / 255.0, 1.0, 1.0);
    let at = |x: i32, y: i32, w: i32, h: i32| -> Rectangle<i32, smithay::utils::Physical> {
        Rectangle::new((r.loc.x + x, r.loc.y + y).into(), (w, h).into())
    };
    thread_local! {
        static IDS: [Id; 4] = std::array::from_fn(|_| Id::new());
    }
    IDS.with(|ids| {
        [
            at(0, 0, w, b),
            at(0, h - b, w, b),
            at(0, b, b, h - 2 * b),
            at(w - b, b, b, h - 2 * b),
        ]
        .into_iter()
        .zip(ids.iter())
        .map(|(geo, id)| {
            SolidColorRenderElement::new(id.clone(), geo, 0usize, color, Kind::Unspecified)
        })
        .collect()
    })
}

/// Window and layer-shell elements for one output whose top-left sits
/// at `offset` in the global space. Empty while content is hidden
/// (locked): nothing beneath the lock surface may show. `split` is the
/// window being dragged over a snap edge: it and everything above it
/// draw over the tile preview.
#[allow(clippy::too_many_arguments)]
fn scene_elements(
    renderer: &mut GlesRenderer,
    manager: &WindowManager,
    state: &State,
    view: View,
    show_content: bool,
    overview: Option<&crate::overview::OverviewLayout>,
    lock: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    split: Option<&smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
) -> Scene {
    if !show_content {
        // Locked: only the shell's lock surface for this output, at its
        // origin (ext-session-lock); nothing else may show.
        return Scene::flat(
            lock.map(|surface| {
                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    (0, 0),
                    view.scale,
                    1.0,
                    Kind::Unspecified,
                )
            })
            .unwrap_or_default(),
        );
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
        return Scene::flat(crate::layer::front_to_back(elements));
    }
    let mut elements: Vec<PreviewElement> = Vec::new();
    // Bottom-to-top index where the dragged window starts.
    let mut split_from = None;
    let tree = |renderer: &mut GlesRenderer,
                elements: &mut Vec<PreviewElement>,
                surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                at: Point<i32, Logical>,
                sx: f64| {
        let origin = view.physical(f64::from(at.x), f64::from(at.y));
        elements.extend(
            render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
                renderer,
                surface,
                origin,
                view.scale,
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(|element| {
                smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                    element,
                    origin,
                    (sx, 1.0),
                )
            }),
        );
    };
    for (window, geometry) in manager.render_windows() {
        // Unassociated X11 windows contribute no surface yet and
        // render nothing this frame.
        if let Some(surface) = window.wl_surface() {
            if split_from.is_none() && split.is_some_and(|s| *s == *surface) {
                split_from = Some(elements.len());
            }
            let origin = crate::popup::surface_origin(&surface, geometry.loc);
            let committed_width = window.geometry().size.w;
            let sx = if manager.session_mode() == crate::windows::SessionMode::Scroll
                && committed_width > 0
            {
                f64::from(geometry.size.w) / f64::from(committed_width)
            } else {
                // An unmapped tree has no committed bounds to scale yet.
                1.0
            };
            tree(renderer, &mut elements, &surface, origin, sx);
            // Popups (#88) right above their window.
            for popup in crate::popup::placed_popups(&surface, origin, true) {
                tree(renderer, &mut elements, &popup.surface, popup.origin, 1.0);
            }
        }
    }
    // Layer shell above windows: panel strip, then overview, each with
    // its popups (toolkit popovers hang off layer surfaces).
    for (surface, (x, y), _) in crate::layer::layer_layout(state) {
        tree(renderer, &mut elements, &surface, (x, y).into(), 1.0);
        for popup in crate::popup::placed_popups(&surface, (x, y).into(), false) {
            tree(renderer, &mut elements, &popup.surface, popup.origin, 1.0);
        }
    }
    // `elements` accumulates bottom-to-top (windows, then
    // background-to-overlay layers); Smithay 0.7 draws the first
    // element topmost.
    let above = elements.len() - split_from.unwrap_or(0);
    Scene {
        elements: crate::layer::front_to_back(elements),
        above,
        tile: Vec::new(),
        top: Vec::new(),
    }
}

/// What lies under the windows on one output, in physical pixels: the
/// wallpaper on the desktop, or in the overview GNOME's workspace cards
/// (the wallpaper below the bar, rounded, over a shadow).
fn backdrop(
    wallpaper: &mut Wallpaper,
    renderer: &mut GlesRenderer,
    size: smithay::utils::Size<i32, smithay::utils::Physical>,
    view: View,
    show_paper: bool,
    cards: Option<&crate::overview::OverviewLayout>,
    locked: bool,
) -> Vec<smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<GlesRenderer>>
{
    if locked {
        return wallpaper
            .lock_element(renderer, size.w, size.h)
            .into_iter()
            .collect();
    }
    if show_paper {
        return wallpaper
            .element(renderer, size.w, size.h)
            .into_iter()
            .collect();
    }
    let Some(layout) = cards else {
        return Vec::new();
    };
    let output = (size.w, size.h).into();
    let work_top = (f64::from(crate::windows::WORK_AREA_TOP) * view.scale).round() as i32;
    layout
        .cards
        .iter()
        .filter_map(|card| {
            let r = card.rect;
            let loc = view.physical(f64::from(r.loc.x), f64::from(r.loc.y));
            let end = view.physical(f64::from(r.loc.x + r.size.w), f64::from(r.loc.y + r.size.h));
            let rect = Rectangle::new(loc, (end.x - loc.x, end.y - loc.y).into());
            wallpaper.card_element(renderer, output, work_top, rect, card.alpha)
        })
        .collect()
}

/// Clear, wallpaper (own pass: mixing element types in one list needs
/// DMA import bounds this backend does not satisfy), then the scene.
/// What one frame redraws, and at which scale.
#[derive(Debug, Clone, Copy)]
struct Target {
    damage: Rectangle<i32, smithay::utils::Physical>,
    scale: f64,
    blank_alpha: f32,
}

fn draw_scene(
    frame: &mut smithay::backend::renderer::gles::GlesFrame<'_, '_>,
    background: Color32F,
    paper: &[smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement<
        GlesRenderer,
    >],
    scene: &Scene,
    target: Target,
    decor: &[(Color32F, Vec<Rectangle<i32, smithay::utils::Physical>>)],
    previews: &[PreviewElement],
) -> Result<(), RuntimeError> {
    let Target {
        damage,
        scale,
        blank_alpha,
    } = target;
    frame
        .clear(background, &[damage])
        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    if !paper.is_empty() {
        draw_render_elements(frame, 1.0, paper, &[damage])
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
    // Beneath the tile preview, the preview (blended), then the
    // dragged window and the layers over it.
    let (over, under) = scene
        .elements
        .split_at(scene.above.min(scene.elements.len()));
    if !under.is_empty() {
        draw_render_elements(frame, scale, under, &[damage])
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    }
    if !scene.tile.is_empty() {
        draw_render_elements::<GlesRenderer, _, _>(frame, 1.0, &scene.tile, &[damage])
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    }
    draw_render_elements(frame, scale, over, &[damage])
        .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    if !scene.top.is_empty() {
        draw_render_elements(frame, scale, &scene.top, &[damage])
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    }
    if blank_alpha > 0.0 {
        use smithay::backend::renderer::element::{solid::SolidColorRenderElement, Id, Kind};
        thread_local! { static SHIELD: Id = Id::new(); }
        let shield = SolidColorRenderElement::new(
            SHIELD.with(Clone::clone),
            damage,
            0usize,
            Color32F::new(0.0, 0.0, 0.0, blank_alpha),
            Kind::Unspecified,
        );
        draw_render_elements::<GlesRenderer, _, _>(frame, 1.0, &[shield], &[damage])
            .map_err(|e| RuntimeError::Dispatch(e.to_string()))?;
    }
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

fn send_frame_callbacks(state: &State, manager: &WindowManager) {
    // Frame callback time measures elapsed time, independently of refresh
    // rate, idle periods, or how many frames the backend has queued.
    let time = Duration::from(state.presentation_now());
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
    // A drag's icon animates on frame callbacks too.
    if let Some(icon) = state.dnd_icon() {
        send_frames_surface_tree(&icon, &output, time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    // The lock screen (ext-session-lock) paints on frame callbacks too.
    for surface in state.lock_surfaces() {
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
    #[cfg(feature = "drm")]
    let _session_services = if matches!(runtime.backend, Backend::Drm(_)) {
        Some(crate::session_services::publish(&session.socket_name))
    } else {
        None
    };
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
