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
use std::os::unix::io::AsFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, BindError, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_output::WlOutput, wl_registry,
        wl_shm::Format, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{
        Anchor, Event as LayerSurfaceEvent, KeyboardInteractivity, ZwlrLayerSurfaceV1,
    },
};

use std::sync::Arc;

use crate::apps::{AppProvider, LaunchTracker};
use crate::control::{ControlClient, ControlError, Handled};
use crate::favorites::Favorites;
use crate::model::ShellModel;
use crate::overview::{
    paint_panel, OverviewCanvas, SwitcherCanvas, BYTES_PER_PIXEL, SWITCHER_STRIP_H,
};
use crate::search::{SearchAction, SearchHub, SearchResult, WindowProvider};

/// Namespace advertised for the panel layer surface.
pub const PANEL_NAMESPACE: &str = "rwd-shell-panel";
/// Namespace advertised for the overview layer surface.
pub const OVERVIEW_NAMESPACE: &str = "rwd-shell-overview";
/// Namespace advertised for the Alt-Tab switcher layer surface.
pub const SWITCHER_NAMESPACE: &str = "rwd-shell-switcher";
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
    /// Compositor does not offer `wl_shm` (the shell draws its
    /// surfaces into shared-memory buffers).
    NoShm(BindError),
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
            Self::NoShm(err) => write!(f, "compositor offers no wl_shm: {err}"),
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
    /// Desktop entries backing launch hits (shared with the hub).
    apps: Arc<AppProvider>,
    /// Live-window snapshot backing switch-to-instance hits (shared
    /// with the hub; refreshed on every [`sync_overview`](Self::sync_overview)).
    windows: Arc<WindowProvider>,
    /// Async bounded search over the providers above.
    search: SearchHub,
    /// Pinned favorites for the overview grid.
    favorites: Favorites,
    /// Launch feedback for spawned apps.
    launcher: LaunchTracker,
    /// Wayland globals for dynamic surface management (`None` in
    /// unit tests, which never touch the wire).
    wayland: Option<WaylandHandles>,
    /// Live overview layer surface while the intent is open.
    overview: Option<OverviewSurface>,
    /// What the overview buffer currently shows; repaint on change.
    paint_key: Option<PaintKey>,
    /// Live switcher layer surface while Alt-Tab is held.
    switcher: Option<SwitcherSurface>,
    /// What the switcher buffer currently shows; repaint on change.
    switcher_key: Option<SwitcherPaintKey>,
    /// Panel size last painted (repaint on configure resize).
    panel_size: Option<(i32, i32)>,
    /// Panel buffer backing (pool fd must outlive the buffer).
    panel_backing: Option<ShmBacking>,
}

/// Cloned Wayland globals the host keeps for creating surfaces after
/// startup (the overview appears and disappears with the intent).
#[derive(Debug, Clone)]
struct WaylandHandles {
    compositor: WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    shm: WlShm,
    qh: QueueHandle<ShellHost>,
}

/// One shm buffer plus everything that must outlive it: the pool
/// and, crucially, the memfd itself (the compositor maps it; dropping
/// the fd first corrupts the scanout).
struct ShmBacking {
    _file: std::fs::File,
    _pool: WlShmPool,
    buffer: WlBuffer,
}

/// Live overview layer surface and its configured size.
struct OverviewSurface {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    backing: Option<ShmBacking>,
    width: i32,
    height: i32,
}

/// Repaint the overview when any of these change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaintKey {
    revision: Option<u64>,
    selected: Option<u64>,
    windows: usize,
    favorites: usize,
    active_workspace: u32,
    width: i32,
    height: i32,
}

/// Live switcher layer surface and its configured size.
struct SwitcherSurface {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    backing: Option<ShmBacking>,
    width: i32,
    height: i32,
}

/// Repaint the switcher strip when any of these change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SwitcherPaintKey {
    selection: Option<u64>,
    entries: usize,
    width: i32,
    height: i32,
}

/// Upload `pixels` (`Argb8888`, `width` x `height`) into a fresh shm
/// buffer. The returned backing owns the memfd: drop it only after
/// the buffer is detached or replaced.
fn shm_upload(
    shm: &WlShm,
    qh: &QueueHandle<ShellHost>,
    pixels: &[u8],
    width: i32,
    height: i32,
) -> Option<ShmBacking> {
    if pixels.is_empty() || width <= 0 || height <= 0 {
        return None;
    }
    let fd = rustix::fs::memfd_create("rwd-shm", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
    let mut file = std::fs::File::from(fd);
    file.set_len(pixels.len() as u64).ok()?;
    use std::io::Write;
    file.write_all(pixels).ok()?;
    let pool = shm.create_pool(file.as_fd(), pixels.len() as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        width,
        height,
        width * BYTES_PER_PIXEL as i32,
        Format::Argb8888,
        qh,
        (),
    );
    Some(ShmBacking {
        _file: file,
        _pool: pool,
        buffer,
    })
}

/// What activating one overview search hit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitOutcome {
    /// Window focused through the token gate (request id for
    /// `CommandResult` correlation); the overview dismisses after.
    Focused {
        /// Activation command request id.
        request: u64,
    },
    /// App spawned (OS pid); feedback flows from
    /// [`ShellHost::launch_states`].
    Launched {
        /// Spawned process id.
        pid: u32,
    },
}

/// Ways hit activation can fail without touching focus.
#[derive(Debug)]
pub enum HitError {
    /// Control channel failed (activation or dismissal send).
    Control(ControlError),
    /// Spawn failed; nothing was launched.
    Launch(std::io::Error),
    /// The focused window left the model between search and Enter.
    StaleWindow {
        /// Compositor window id with no live entry.
        window: u64,
    },
    /// The launched app id has no known desktop entry.
    UnknownApp {
        /// Requested app id.
        app_id: String,
    },
}

impl fmt::Display for HitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control(err) => write!(f, "overview command failed: {err}"),
            Self::Launch(err) => write!(f, "launch failed: {err}"),
            Self::StaleWindow { window } => {
                write!(f, "window {window} closed before activation")
            }
            Self::UnknownApp { app_id } => write!(f, "no desktop entry for {app_id}"),
        }
    }
}

impl std::error::Error for HitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Control(err) => Some(err),
            Self::Launch(err) => Some(err),
            Self::StaleWindow { .. } | Self::UnknownApp { .. } => None,
        }
    }
}

impl ShellHost {
    /// Host over explicit workflow state (002 T2).
    ///
    /// Providers merge windows first (switch-to-instance beats fresh
    /// launch), then desktop entries. Production passes
    /// [`AppProvider::system`](crate::apps::AppProvider::system) and
    /// [`Favorites::system`](crate::favorites::Favorites::system);
    /// tests inject temp-dir state.
    pub fn new(panel: PanelConfig, apps: AppProvider, favorites: Favorites) -> Self {
        let windows: Arc<WindowProvider> = Arc::new(WindowProvider::new());
        let apps: Arc<AppProvider> = Arc::new(apps);
        let search = SearchHub::new(vec![
            windows.clone() as Arc<dyn crate::search::SearchProvider>,
            apps.clone() as Arc<dyn crate::search::SearchProvider>,
        ]);
        Self {
            model: ShellModel::new(),
            panel,
            running: true,
            surface: None,
            layer_surface: None,
            apps,
            windows,
            search,
            favorites,
            launcher: LaunchTracker::new(),
            wayland: None,
            overview: None,
            paint_key: None,
            switcher: None,
            switcher_key: None,
            panel_size: None,
            panel_backing: None,
        }
    }

    /// Hand the host the Wayland globals after binding (the run
    /// functions call this; tests leave `wayland` empty and every
    /// surface op below no-ops without it).
    pub fn attach_wayland(
        &mut self,
        compositor: WlCompositor,
        layer_shell: ZwlrLayerShellV1,
        shm: WlShm,
        qh: QueueHandle<ShellHost>,
    ) {
        self.wayland = Some(WaylandHandles {
            compositor,
            layer_shell,
            shm,
            qh,
        });
    }

    /// Reconcile the overview surface with the intent (call after
    /// every [`sync_overview`](Self::sync_overview)): create the
    /// layer surface when the intent opens, destroy it when it
    /// closes, repaint when model truth or the configured size
    /// changes. Pure no-op without attached Wayland globals.
    /// Tear down the overview surface, if any. Destructor requests go
    /// out explicitly: dropping the proxies alone never notifies the
    /// server, which would keep scanning out the orphaned surface.
    fn destroy_overview(&mut self) {
        if let Some(overview) = self.overview.take() {
            overview.layer.destroy();
            overview.surface.destroy();
        }
        self.paint_key = None;
    }

    /// Tear down the switcher surface, if any. Same explicit-destroy
    /// rule as the overview: proxies alone never notify the server.
    fn destroy_switcher(&mut self) {
        if let Some(switcher) = self.switcher.take() {
            switcher.layer.destroy();
            switcher.surface.destroy();
        }
        self.switcher_key = None;
    }

    /// Reconcile the switcher surface with the model overlay (call
    /// after every [`sync_overview`](Self::sync_overview)): create the
    /// bottom-anchored strip when Alt-Tab opens, destroy it on commit
    /// or cancel, repaint when the MRU selection or size changes.
    /// Pure no-op without attached Wayland globals.
    fn update_switcher(&mut self) {
        if !self.model.is_switcher_open() {
            self.destroy_switcher();
            return;
        }
        let Some(wayland) = self.wayland.clone() else {
            return;
        };
        if self.switcher.is_none() {
            let surface = wayland.compositor.create_surface(&wayland.qh, ());
            let layer = wayland.layer_shell.get_layer_surface(
                &surface,
                None,
                Layer::Overlay,
                SWITCHER_NAMESPACE.to_owned(),
                &wayland.qh,
                (),
            );
            layer.set_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right);
            layer.set_size(0, SWITCHER_STRIP_H as u32);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();
            self.switcher = Some(SwitcherSurface {
                surface,
                layer,
                backing: None,
                width: 0,
                height: 0,
            });
            self.switcher_key = None;
        }
        let switcher = self.switcher.as_mut().expect("created above");
        if switcher.width <= 0 || switcher.height <= 0 {
            return;
        }
        let key = SwitcherPaintKey {
            selection: self.model.switcher_selection(),
            entries: self.model.mru_order().len(),
            width: switcher.width,
            height: switcher.height,
        };
        if self.switcher_key == Some(key) {
            return;
        }
        let mut canvas = SwitcherCanvas::new(switcher.width, switcher.height);
        canvas.render(self.model.mru_order(), self.model.switcher_selection());
        if let Some(backing) = shm_upload(
            &wayland.shm,
            &wayland.qh,
            canvas.pixels(),
            switcher.width,
            switcher.height,
        ) {
            switcher.surface.attach(Some(&backing.buffer), 0, 0);
            switcher
                .surface
                .damage(0, 0, switcher.width, switcher.height);
            switcher.surface.commit();
            switcher.backing = Some(backing);
            self.switcher_key = Some(key);
        }
    }

    fn update_overview(&mut self, revision: Option<u64>) {
        if !self.model.is_overview_open() {
            self.destroy_overview();
            return;
        }
        let Some(wayland) = self.wayland.clone() else {
            return;
        };
        if self.overview.is_none() {
            let surface = wayland.compositor.create_surface(&wayland.qh, ());
            let layer = wayland.layer_shell.get_layer_surface(
                &surface,
                None,
                Layer::Overlay,
                OVERVIEW_NAMESPACE.to_owned(),
                &wayland.qh,
                (),
            );
            layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            surface.commit();
            self.overview = Some(OverviewSurface {
                surface,
                layer,
                backing: None,
                width: 0,
                height: 0,
            });
            self.paint_key = None;
        }
        let overview = self.overview.as_mut().expect("created above");
        if overview.width <= 0 || overview.height <= 0 {
            return;
        }
        let key = PaintKey {
            revision,
            selected: self.model.selected(),
            windows: self.model.windows().len(),
            favorites: self.favorites.ids().len(),
            active_workspace: self.model.active_workspace(),
            width: overview.width,
            height: overview.height,
        };
        if self.paint_key == Some(key) {
            return;
        }
        let mut canvas = OverviewCanvas::new(overview.width, overview.height);
        canvas.render(&self.model, self.favorites.ids().len());
        if let Some(backing) = shm_upload(
            &wayland.shm,
            &wayland.qh,
            canvas.pixels(),
            overview.width,
            overview.height,
        ) {
            overview.surface.attach(Some(&backing.buffer), 0, 0);
            overview
                .surface
                .damage(0, 0, overview.width, overview.height);
            overview.surface.commit();
            // The old backing drops here, after the new buffer is
            // committed — the compositor never reads a freed mapping.
            overview.backing = Some(backing);
            self.paint_key = Some(key);
        }
    }

    /// Paint the panel strip into a fresh shm buffer and attach it.
    fn paint_panel_surface(&mut self, width: i32) {
        let height = self.panel.height as i32;
        if width <= 0 || height <= 0 {
            return;
        }
        if self.panel_size == Some((width, height)) && self.panel_backing.is_some() {
            return;
        }
        let Some(wayland) = self.wayland.clone() else {
            return;
        };
        let mut pixels = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL];
        paint_panel(&mut pixels, width, height);
        let Some(surface) = self.surface.as_ref() else {
            return;
        };
        if let Some(backing) = shm_upload(&wayland.shm, &wayland.qh, &pixels, width, height) {
            surface.attach(Some(&backing.buffer), 0, 0);
            surface.damage(0, 0, width, height);
            surface.commit();
            self.panel_backing = Some(backing);
            self.panel_size = Some((width, height));
        }
    }

    /// Pinned favorites for the overview grid.
    pub fn favorites(&self) -> &Favorites {
        &self.favorites
    }

    /// Pin or unpin a favorite (persistence is the caller's `save`).
    pub fn favorites_mut(&mut self) -> &mut Favorites {
        &mut self.favorites
    }

    /// Start answering overview search text; returns the generation.
    /// Never blocks: providers resolve on worker threads.
    pub fn search_query(&self, text: &str) -> u64 {
        self.search.query(text)
    }

    /// Drop in-flight answers (e.g. overview closed mid-query).
    pub fn search_cancel(&self) {
        self.search.cancel();
    }

    /// Nonblocking drain of the current answer, merged and bounded.
    pub fn search_collect(&mut self) -> Vec<SearchResult> {
        self.search.collect()
    }

    /// Current launch feedback per app id (reaps exited children).
    pub fn launch_states(
        &mut self,
    ) -> std::collections::HashMap<String, Vec<crate::apps::LaunchState>> {
        self.launcher.states()
    }

    /// Activate one search hit (002 T2 Enter behavior).
    ///
    /// Focus hits select the window in the host model and activate
    /// through the token gate, then ask the compositor to dismiss the
    /// overview (GNOME parallel: picking from the overview returns to
    /// the app). Launch hits spawn detached and dismiss best-effort:
    /// a dismissal send failure after a successful spawn must not
    /// report the launch as failed.
    pub fn activate_hit(
        &mut self,
        control: &mut ControlClient,
        hit: &SearchResult,
    ) -> Result<HitOutcome, HitError> {
        match &hit.action {
            SearchAction::Focus { window } => {
                if !self.model.select_window(*window) {
                    return Err(HitError::StaleWindow { window: *window });
                }
                let request = control
                    .activate_window(*window)
                    .map_err(HitError::Control)?;
                control.toggle_overview().map_err(HitError::Control)?;
                Ok(HitOutcome::Focused { request })
            }
            SearchAction::Launch { app_id } => {
                let entry = self
                    .apps
                    .entry(app_id)
                    .ok_or_else(|| HitError::UnknownApp {
                        app_id: app_id.clone(),
                    })?;
                let pid = self.launcher.launch(entry).map_err(HitError::Launch)?;
                let _ = control.toggle_overview();
                Ok(HitOutcome::Launched { pid })
            }
        }
    }
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

    /// Replace the overview state with the control client's live model:
    /// current window list, workspaces, selection, and the compositor's
    /// overview-open intent (002 R1). The panel renders only this
    /// compositor truth.
    fn sync_overview(&mut self, client: &ControlClient) {
        let model = client.model();
        self.model
            .apply_window_list(model.windows().to_vec(), model.workspaces().to_vec());
        self.model.set_active_workspace(model.active_workspace());
        if let Some(selected) = model.selected() {
            let _ = self.model.select_window(selected);
        }
        self.model.set_overview_open(model.is_overview_open());
        self.model
            .apply_switcher_state(model.is_switcher_open(), model.switcher_selection());
        // Keep switch-to-instance answers on compositor truth.
        self.windows.refresh(&self.model);
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
            LayerSurfaceEvent::Configure {
                serial,
                width,
                height,
            } => {
                if state.layer_surface.as_ref() == Some(proxy) {
                    proxy.ack_configure(serial);
                    // First configure carries the arranged width: paint
                    // the strip and commit the buffer with it.
                    state.paint_panel_surface(width as i32);
                    if let Some(surface) = state.surface.as_ref() {
                        surface.commit();
                    }
                } else if state
                    .overview
                    .as_ref()
                    .is_some_and(|overview| &overview.layer == proxy)
                {
                    proxy.ack_configure(serial);
                    // Size arrives here; the next update pass paints.
                    if let Some(overview) = state.overview.as_mut() {
                        if width > 0 && height > 0 {
                            overview.width = width as i32;
                            overview.height = height as i32;
                        }
                    }
                } else if state
                    .switcher
                    .as_ref()
                    .is_some_and(|switcher| &switcher.layer == proxy)
                {
                    proxy.ack_configure(serial);
                    // Size arrives here; the next update pass paints.
                    if let Some(switcher) = state.switcher.as_mut() {
                        if width > 0 && height > 0 {
                            switcher.width = width as i32;
                            switcher.height = height as i32;
                        }
                    }
                } else {
                    proxy.ack_configure(serial);
                }
            }
            LayerSurfaceEvent::Closed => {
                if state.layer_surface.as_ref() == Some(proxy) {
                    // Losing the panel ends the loop (supervised restart
                    // brings it back); losing the overview just drops it.
                    state.running = false;
                } else if state
                    .overview
                    .as_ref()
                    .is_some_and(|overview| &overview.layer == proxy)
                {
                    state.destroy_overview();
                } else if state
                    .switcher
                    .as_ref()
                    .is_some_and(|switcher| &switcher.layer == proxy)
                {
                    state.destroy_switcher();
                }
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
delegate_noop!(ShellHost: ignore WlShm);
delegate_noop!(ShellHost: ignore WlShmPool);
delegate_noop!(ShellHost: ignore WlBuffer);

/// Connect plus event-loop setup kept together for `main`: builds the
/// connection, queue, and host, then dispatches until close.
pub fn run_panel(panel: PanelConfig) -> Result<(), PanelError> {
    run_panel_with_control(panel, None)
}

/// Nonblocking Wayland pump for the control-paced loop below:
/// `dispatch_pending` dispatches queued events but never reads the
/// socket, so a zero-timeout poll gates one socket read per iteration.
/// Without this, server events (e.g. overview configures) wait in the
/// kernel buffer until the next `roundtrip`/`blocking_dispatch`.
fn pump_wayland(
    conn: &Connection,
    queue: &mut wayland_client::EventQueue<ShellHost>,
    host: &mut ShellHost,
) -> Result<(), PanelError> {
    let backend = conn.backend();
    let fd = backend.poll_fd();
    let mut fds = [rustix::event::PollFd::new(
        &fd,
        rustix::event::PollFlags::IN,
    )];
    let readable = rustix::event::poll(&mut fds, 0)
        .map(|n| n > 0)
        .unwrap_or(false);
    if readable {
        if let Some(guard) = queue.prepare_read() {
            guard.read().map_err(|e| PanelError::Flush(e.to_string()))?;
        }
    }
    queue.dispatch_pending(host).map_err(PanelError::Dispatch)?;
    Ok(())
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
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(PanelError::NoShm)?;

    let mut host = ShellHost::new(panel, AppProvider::system(), Favorites::system());
    host.attach_wayland(compositor.clone(), layer_shell.clone(), shm, qh.clone());
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
            pump_wayland(&conn, &mut queue, &mut host)?;
            drive_control(control, &mut host);
            host.update_overview(control.revision());
            host.update_switcher();
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
        use std::rc::Rc;

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

        use super::super::{is_would_block, PanelConfig, ShellHost, PANEL_NAMESPACE};
        use crate::apps::AppProvider;
        use crate::favorites::Favorites;

        const PUMP_ROUNDS: usize = 200;

        /// Host with empty entries and temp-dir favorites: no HOME or
        /// system app-dir side effects in tests. The guard keeps the
        /// dir (and the favorites file) alive for the test body.
        fn test_host() -> (ShellHost, tempfile::TempDir) {
            let dir = tempfile::tempdir().expect("tempdir");
            let host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(Vec::new()),
                Favorites::load(dir.path().join(crate::favorites::FAVORITES_FILE)),
            );
            (host, dir)
        }

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

            let (mut host, _dir) = test_host();
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

            let (mut host, _dir) = test_host();
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

        /// Alt-Tab drive round trip: hub-queued steps open the host
        /// switcher on the MRU previous window, commit activates
        /// through the token gate, cancel closes without touching
        /// focus. Same `sync_overview` path the run loop drives.
        #[test]
        fn switcher_drive_steps_and_commits() {
            use rwd_shell_control::{CommandStatus, SwitcherAction};

            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("control.sock");
            let mut model = StateModel::new();
            let id_a = model.insert("alpha", Some("com.example.alpha"), 0);
            let id_b = model.insert("beta", Some("com.example.beta"), 0);
            assert!(model.set_focused(Some(id_a)));
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

            let (mut host, _dir) = test_host();
            host.sync_overview(&client);
            assert_eq!(host.model.mru_order(), &[id_a, id_b]);

            // One step opens the switcher on the MRU previous window.
            hub.queue_switcher(SwitcherAction::Step { forward: true });
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Switcher { selection, .. }) => {
                        assert_eq!(selection, Some(id_b));
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("switcher poll failed: {e}"),
                }
            }
            host.sync_overview(&client);
            assert!(host.model.is_switcher_open());
            assert_eq!(host.model.switcher_selection(), Some(id_b));

            // Commit activates the selection through the token gate.
            hub.queue_switcher(SwitcherAction::Commit);
            let mut committed = false;
            let mut applied = false;
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Switcher {
                        action: SwitcherAction::Commit,
                        ..
                    }) => {
                        committed = true;
                    }
                    Ok(Handled::CommandResult { status, .. }) => {
                        assert_eq!(status, CommandStatus::Applied);
                        applied = true;
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("commit poll failed: {e}"),
                }
                if committed && applied {
                    break;
                }
            }
            assert!(committed, "commit drive arrived");
            assert!(applied, "activation applied");
            assert_eq!(model.focused(), Some(id_b));
            host.sync_overview(&client);
            assert!(!host.model.is_switcher_open());

            // Step then cancel: focus stays where the commit left it.
            hub.queue_switcher(SwitcherAction::Step { forward: true });
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Switcher { .. }) => break,
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("restep poll failed: {e}"),
                }
            }
            hub.queue_switcher(SwitcherAction::Cancel);
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Switcher {
                        action: SwitcherAction::Cancel,
                        ..
                    }) => {
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("cancel poll failed: {e}"),
                }
            }
            assert_eq!(model.focused(), Some(id_b));
            host.sync_overview(&client);
            assert!(!host.model.is_switcher_open());
        }

        /// Search answers through the host: after a sync the window
        /// provider offers switch-to-instance hits for live titles.
        #[test]
        fn host_search_finds_synced_windows() {
            use crate::search::SearchAction;

            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("control.sock");
            let mut model = StateModel::new();
            model.insert("alpha", Some("com.example.alpha"), 0);
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

            let (mut host, _favdir) = test_host();
            host.sync_overview(&client);
            host.search_query("alp");
            let mut hits = Vec::new();
            for _ in 0..100 {
                hits = host.search_collect();
                if !hits.is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(hits.len(), 1, "one live window matches");
            assert!(
                matches!(hits[0].action, SearchAction::Focus { .. }),
                "window hits switch to the instance"
            );
        }

        /// Enter on a window hit focuses through the token gate and
        /// dismisses the overview; the compositor confirms both.
        #[test]
        fn activate_window_hit_focuses_and_dismisses() {
            use crate::search::SearchResult;
            use rwd_shell_control::CommandStatus;

            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("control.sock");
            let mut model = StateModel::new();
            let id_a = model.insert("alpha", Some("com.example.alpha"), 0);
            let id_b = model.insert("beta", Some("com.example.beta"), 0);
            assert!(model.set_focused(Some(id_a)));
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

            let (mut host, _favdir) = test_host();
            host.sync_overview(&client);
            hub.set_overview(true);
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Overview { open: true }) => break,
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("overview poll failed: {e}"),
                }
            }
            host.sync_overview(&client);
            assert!(host.model.is_overview_open());

            let hit = SearchResult::focus("beta", None, id_b);
            let request = match host.activate_hit(&mut client, &hit) {
                Ok(super::super::HitOutcome::Focused { request }) => request,
                other => panic!("expected focus, got {other:?}"),
            };
            assert_eq!(host.model.selected(), Some(id_b));

            // The hub applies the activation, then the dismissal flip.
            // Results are matched by request id: the dismissal toggle
            // has its own id and must not stand in for the activation.
            let mut result = None;
            let mut dismissed = false;
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::CommandResult { id, status }) => {
                        if id == request {
                            result = Some(status);
                        }
                    }
                    Ok(Handled::Overview { open }) => {
                        if !open {
                            dismissed = true;
                        }
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("result poll failed: {e}"),
                }
                if result.is_some() && dismissed {
                    break;
                }
            }
            assert_eq!(result, Some(CommandStatus::Applied));
            assert!(dismissed, "overview dismisses after picking");
            assert_eq!(model.focused(), Some(id_b));
        }

        /// Enter on a closed window reports stale without touching the
        /// control channel.
        #[test]
        fn activate_stale_window_hit_is_not_sent() {
            use crate::search::SearchResult;
            use std::os::unix::net::UnixStream as StdUnixStream;

            let (mut host, _favdir) = test_host();
            let (stream, _peer) = StdUnixStream::pair().expect("socketpair");
            let mut client = ControlClient::new(stream).expect("client");
            let hit = SearchResult::focus("gone", None, 99);
            match host.activate_hit(&mut client, &hit) {
                Err(super::super::HitError::StaleWindow { window }) => {
                    assert_eq!(window, 99);
                }
                other => panic!("expected stale window, got {other:?}"),
            }
        }

        /// Enter on an app hit spawns detached and reports the pid;
        /// unknown ids fail before any spawn.
        #[test]
        fn activate_launch_hit_spawns_and_tracks() {
            use crate::apps::discover;
            use crate::search::SearchResult;

            let apps_dir = tempfile::tempdir().unwrap();
            std::fs::write(
                apps_dir.path().join("run.desktop"),
                "[Desktop Entry]\nName=Runner\nExec=/bin/true\nType=Application\n",
            )
            .unwrap();
            let fav_dir = tempfile::tempdir().unwrap();
            let mut host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(discover(&[apps_dir.path().to_owned()])),
                Favorites::load(fav_dir.path().join(crate::favorites::FAVORITES_FILE)),
            );
            let (stream, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            let mut client = ControlClient::new(stream).expect("client");

            let hit = SearchResult::launch("Runner", "run");
            match host.activate_hit(&mut client, &hit) {
                Ok(super::super::HitOutcome::Launched { pid }) => {
                    assert!(pid > 0);
                }
                other => panic!("expected launch, got {other:?}"),
            }
            let mut exited = false;
            for _ in 0..100 {
                if host.launch_states().get("run").is_some_and(|states| {
                    states
                        .iter()
                        .any(|s| matches!(s, crate::apps::LaunchState::Exited { code: Some(0) }))
                }) {
                    exited = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(exited, "tracker reports the clean exit");

            let bogus = SearchResult::launch("Bogus", "nope.desktop");
            match host.activate_hit(&mut client, &bogus) {
                Err(super::super::HitError::UnknownApp { app_id }) => {
                    assert_eq!(app_id, "nope.desktop");
                }
                other => panic!("expected unknown app, got {other:?}"),
            }
        }
    }
}
