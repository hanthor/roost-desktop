//! Wayland client setup for the supervised shell host's Activities panel.
//!
//! The shell host is a separate Wayland client (001 spec R3/R5, ADR 0003):
//! it connects via `WAYLAND_DISPLAY`, binds `zwlr_layer_shell_v1`, and
//! creates one top-anchored layer surface carrying the Activities trigger
//! and window list. No UI logic runs inside the compositor process.
//!
//! # Live runtime
//!
//! The compositor serves `zwlr_layer_shell_v1` (see `roost_compositor::layer`),
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

use roost_shell_control::OutputInfo;
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, BindError, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_keyboard::{Event as KeyEvent, KeyState, KeymapFormat, WlKeyboard},
        wl_output::WlOutput,
        wl_pointer::{ButtonState, Event as PointerEvent, WlPointer},
        wl_registry,
        wl_seat::{Capability, Event as SeatEvent, WlSeat},
        wl_shm::Format,
        wl_shm::WlShm,
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{Event as XdgOutputEvent, ZxdgOutputV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{
        Anchor, Event as LayerSurfaceEvent, KeyboardInteractivity, ZwlrLayerSurfaceV1,
    },
};

use std::sync::{Arc, Mutex};

use crate::apps::{entry_from_file, AppEntry, AppProvider, LaunchTracker};
use crate::control::{ControlClient, ControlError, Handled};
use crate::dock::{
    dock_items, dock_press, dock_slot_at, paint_dock_stacked_with_icons, read_stack, stack_cell_at,
    stack_shown, DockAction, DockItem, StackEntry, DOCK_H, DOCK_SLOT, STACK_GRID_H,
};
use crate::extensions::{extension_dir, ExtensionHost};
use crate::favorites::Favorites;
use crate::icons::Artwork;
use crate::intake::NotificationBus;
use crate::keyboard::{KeyAction, XkbFeed};
use crate::model::ShellModel;
use crate::notifications::{NotificationAction, NotificationCenter, Urgency};
use crate::overview::{
    banner_hit, banner_strip_height, blit_glyph, glyph_index, overview_press, paint_panel,
    put_pixel, BannerCanvas, BannerHit, BannerRow, OverviewCanvas, SwitcherCanvas, ACCENT,
    BANNER_STRIP_W, BYTES_PER_PIXEL, FONT_SCALE, GLYPH_ADVANCE, SWITCHER_STRIP_H,
};
use crate::popup::{
    calendar_clock_row, calendar_weekday_row, lock_rows, network_rows, paint_popup, panel_layout,
    popup_box, sound_rows, tile_row_at, tile_row_rect, PopupBody, PopupState, Rect, POPUP_HEIGHT,
};
use crate::search::{SearchAction, SearchHub, SearchResult, WindowProvider, MAX_TOTAL_RESULTS};
use crate::settings::ClockFormat;
use crate::tiles::{TileSet, TileState, NETWORK_TILE_INDEX, POWER_TILE_INDEX, SOUND_TILE_INDEX};
use crate::watcher::menu_row_rect;
use crate::watcher::{
    indicator_at, indicator_cells, indicator_right_x, menu_row_at, paint_indicators, IndicatorHost,
    IndicatorIcon, ItemInfo, WatcherBus, INDICATOR_CELL,
};

/// Namespace advertised for the panel layer surface.
pub const PANEL_NAMESPACE: &str = "roost-shell-panel";
/// Layer namespace for the bottom dock surface.
pub const DOCK_NAMESPACE: &str = "roost-shell-dock";
/// Namespace advertised for the overview layer surface.
pub const OVERVIEW_NAMESPACE: &str = "roost-shell-overview";
/// Namespace advertised for the Alt-Tab switcher layer surface.
pub const SWITCHER_NAMESPACE: &str = "roost-shell-switcher";
/// Namespace advertised for the notification banner layer surface.
pub const BANNER_NAMESPACE: &str = "roost-shell-banner";
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
    /// Compositor does not offer `wl_seat` (overview search needs
    /// the keyboard; running deaf would hide that).
    NoSeat(BindError),
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
            Self::NoSeat(err) => write!(f, "compositor offers no wl_seat: {err}"),
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
    /// Local notification center, shared with the intake bus edge
    /// (wire arrivals file here; banners render from it).
    center: Arc<Mutex<NotificationCenter>>,
    /// Live banner layer surface while banners are queued.
    banners: Option<BannerSurface>,
    /// What the banner buffer currently shows; repaint on change.
    banner_key: Option<BannerPaintKey>,
    /// Panel size last painted (repaint on configure resize).
    panel_size: Option<(i32, i32)>,
    /// Status tiles (clock plus service presence), refreshed on a slow
    /// tick so absent services never block the loop.
    tiles: TileSet,
    /// Last tile probe, for throttling (probes are cheap local reads,
    /// but 200 Hz would still be waste).
    tiles_refreshed: Option<std::time::Instant>,
    /// What the panel buffer currently shows; repaint on change.
    panel_paint_key: Option<PanelPaintKey>,
    /// Panel buffer backing (pool fd must outlive the buffer).
    panel_backing: Option<ShmBacking>,
    /// Seat owning our keyboard (`None` in unit tests, which never
    /// touch the wire).
    seat: Option<WlSeat>,
    /// Server keyboard for overview search input.
    keyboard: Option<WlKeyboard>,
    /// Server pointer for panel presses (clock, tiles, popup
    /// dismissal). Acquired on pointer capability like the keyboard.
    pointer: Option<WlPointer>,
    /// Last pointer position on our panel surface, surface coordinates.
    /// Button events carry no coordinates, so motion feeds presses.
    pointer_pos: Option<(f64, f64)>,
    /// True while the pointer is over the dock surface (set on enter,
    /// cleared on panel enter): routes button presses to the dock.
    pointer_on_dock: bool,
    /// True while the pointer is over the banner surface (set on
    /// enter, cleared on panel or dock enter): routes button presses
    /// to [`ShellHost::press_banner`]. The banner never takes keyboard
    /// focus; this flag only steers presses.
    pointer_on_banner: bool,
    /// Open calendar/menu popup, if any.
    popup: PopupState,
    /// Bottom dock layer surface and its arranged size (`None` before
    /// the first configure; unit tests never attach it).
    dock_layer: Option<ZwlrLayerSurfaceV1>,
    dock_surface: Option<WlSurface>,
    dock_size: Option<(i32, i32)>,
    dock_backing: Option<ShmBacking>,
    dock_paint_key: Option<DockPaintKey>,
    /// Output the primary panel is bound to (`None` while the legacy
    /// unbound surface from startup is live).
    panel_output: Option<String>,
    /// Output the dock is bound to (`None` while legacy unbound).
    dock_output: Option<String>,
    /// Bound `wl_output` globals with their compositor names (`None`
    /// until the `xdg_output` name event resolves). Filled from
    /// registry events; the tick reconciles surfaces against it plus
    /// the control inventory.
    bound_outputs: Vec<BoundOutput>,
    /// Extra panel surfaces for non-primary outputs (primary keeps
    /// the legacy `surface`/`panel_*` fields above, so presses and
    /// popups stay on one surface).
    extra_panels: Vec<ExtraPanel>,
    /// `xdg_output` manager for naming bound outputs (`None` until
    /// the registry advertises it; unit tests never attach it).
    xdg_manager: Option<ZxdgOutputManagerV1>,
    /// Open folder-stack grid: item index into the current dock
    /// items, plus the directory read cached at open time.
    open_stack: Option<usize>,
    stack_cache: Vec<StackEntry>,
    /// Last wallpaper URI published to the compositor drop file.
    published_wallpaper: Option<String>,
    /// Hosted app indicators (StatusNotifier items).
    indicators: IndicatorHost,
    /// Sandboxed extension scripts and their cached outputs.
    extensions: ExtensionHost,
    /// Per-script note surfacing state: last seen enabled flag plus
    /// notice texts already shown, so each note banners exactly once.
    ext_seen: std::collections::HashMap<String, ExtSeen>,
    /// Press action ids queued by strip presses, drained into script
    /// handlers on the slow tick so no script code runs on the event
    /// path itself.
    pending_presses: Vec<String>,
    /// D-Bus edge behind the indicator host.
    watcher: WatcherBus,
    /// D-Bus edge behind the notification center.
    notifications: NotificationBus,
    /// Resolved icon artwork keyed (theme, name, size). Cleared
    /// wholesale when the snapshot theme changes.
    icon_cache: std::collections::HashMap<(String, String, u32), Artwork>,
    /// Theme the cache was built under.
    icon_theme: String,
    /// Dock switch/close actions awaiting the control client (the run
    /// loop's driver consumes them; launch and pin run immediately).
    pending_dock: Vec<DockAction>,
    /// Manual lock request armed by a power-menu press (the run
    /// loop's driver sends it; the lock screen engages when the
    /// compositor's locked snapshot lands).
    pending_lock: bool,
    /// xkb state behind the keyboard; `None` until the first keymap
    /// arrives, while which keys are ignored.
    xkb: Option<XkbFeed>,
    /// Live overview search text (re-queried on every edit).
    search_text: String,
    /// Pressed overview result whose spawn failed (row index plus
    /// the spawn error text). Set on `HitError::Launch` in
    /// [`press_overview`](Self::press_overview) — the overview stays
    /// open and the next repaint marks the row via
    /// [`OverviewCanvas::draw_launch_failure`](crate::overview::OverviewCanvas::draw_launch_failure).
    /// Cleared on successful press, fresh query text, and overview
    /// close so a stale row never paints.
    overview_failure: Option<OverviewFailure>,
    /// Enter arrived while open: the run loop activates the top hit.
    pending_submit: bool,
    /// Escape arrived while open: the run loop dismisses the overview.
    pending_dismiss: bool,
    /// Navigation actions armed by [`on_key`](Self::on_key); the
    /// focus model drains them with [`take_nav`](Self::take_nav).
    pending_nav: Vec<KeyAction>,
    /// Enter pressed with the overview closed: the run loop routes
    /// it to the focused shell stop (the overview owns Enter while
    /// open through [`take_submit`](Self::take_submit), unchanged).
    pending_shell_submit: bool,
    /// Keyboard focus cursor (`None` until the first focus key):
    /// region plus stop index. Paint keys carry it, so any focus
    /// change repaints by construction.
    focus: Option<ShellFocus>,
    /// Parked cursors under open transient surfaces: opening a
    /// popup or the overview pushes the live cursor here, and
    /// Escape pops back to it. Bounded; only transient entries
    /// (popup, overview) are pushed, never region jumps.
    return_stack: Vec<ShellFocus>,
    /// Latest collected search hits behind the overview result
    /// cursor, refreshed from the hub. Cleared on every re-query
    /// and overview close so the cursor never indexes stale rows.
    overview_hits: Vec<SearchResult>,
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

/// Keyboard focus region: the panel strip, the dock, the open
/// popup's rows, or the overview's result list. F6 cycles Panel
/// and Dock; Popup and Overview take focus when they open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusRegion {
    /// Top strip: clock, the three tiles, then hosted indicator
    /// cells in strip order.
    Panel,
    /// Bottom dock: favorite and running slots.
    Dock,
    /// Open popup: calendar footers or menu rows.
    Popup,
    /// Open overview: search result rows.
    Overview,
}

/// Panel strip stops before hosted indicators: the clock plus the
/// three service tiles in strip order (network, power, sound).
/// Indicator cells append after the tiles while hosted.
pub const PANEL_STOPS: usize = 4;

/// Keyboard cursor: region plus the stop index within it. Panel
/// stops run left-to-right over clock and tiles; dock stops run
/// over the item slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellFocus {
    /// Focused region.
    pub region: FocusRegion,
    /// Stop index within the region.
    pub cursor: usize,
}

/// 2px accent outline around `rect`, clamped to the buffer by the
/// bounds-safe pixel writer: the keyboard-focus ring, mirroring
/// the overview selection border.
fn paint_focus_ring(pixels: &mut [u8], width: i32, rect: &Rect) {
    let stride = width as usize * BYTES_PER_PIXEL;
    for x in rect.x - 2..rect.x + rect.w + 2 {
        for y in [rect.y - 2, rect.y - 1, rect.y + rect.h, rect.y + rect.h + 1] {
            put_pixel(pixels, stride, x, y, ACCENT);
        }
    }
    for y in rect.y - 2..rect.y + rect.h + 2 {
        for x in [rect.x - 2, rect.x - 1, rect.x + rect.w, rect.x + rect.w + 1] {
            put_pixel(pixels, stride, x, y, ACCENT);
        }
    }
}

/// Paint extension script badges on the dock row: one framed slot
/// per badge with its text truncated to fit, starting at `start_x`.
/// Cached texts only — no script runs on this path.
fn paint_script_badges(
    pixels: &mut [u8],
    width: i32,
    start_x: i32,
    y_origin: i32,
    badges: &[(String, String)],
) {
    const DIM: [u8; 4] = [0x4a, 0x44, 0x44, 0xff];
    let stride = width as usize * BYTES_PER_PIXEL;
    let max_chars = ((DOCK_SLOT - 8) / GLYPH_ADVANCE).max(1) as usize;
    for (index, (_, text)) in badges.iter().enumerate() {
        let x0 = start_x + index as i32 * DOCK_SLOT;
        for dx in 0..DOCK_SLOT {
            put_pixel(pixels, stride, x0 + dx, y_origin, DIM);
            put_pixel(pixels, stride, x0 + dx, y_origin + DOCK_H - 1, DIM);
        }
        let mut gx = x0 + 4;
        let gy = y_origin + 6;
        for ch in text.chars().take(max_chars) {
            if ch == ' ' {
                gx += GLYPH_ADVANCE;
                continue;
            }
            if let Some(glyph) = glyph_index(ch) {
                blit_glyph(pixels, stride, gx, gy, glyph, ACCENT);
            }
            gx += GLYPH_ADVANCE;
        }
    }
}

/// Paint extension script cells: a frame plus the cell text's
/// initial, mirroring the indicator look. Cached texts only — no
/// script runs on this path.
fn paint_script_cells(pixels: &mut [u8], width: i32, cells: &[Rect], texts: &[&str]) {
    let stride = width as usize * BYTES_PER_PIXEL;
    for (cell, text) in cells.iter().zip(texts.iter()) {
        for dx in 0..cell.w {
            put_pixel(pixels, stride, cell.x + dx, cell.y, ACCENT);
            put_pixel(pixels, stride, cell.x + dx, cell.y + cell.h - 1, ACCENT);
        }
        if let Some(initial) = text.chars().next() {
            if let Some(glyph) = glyph_index(initial) {
                let gx = cell.x + (cell.w - 4 * FONT_SCALE) / 2;
                let gy = cell.y + (cell.h - 5 * FONT_SCALE) / 2;
                blit_glyph(pixels, stride, gx, gy, glyph, ACCENT);
            }
        }
    }
}

/// What distinguishes one shell layer surface from another. Everything
/// else about making one is identical, so it lives in
/// [`make_layer_surface`] and nowhere else.
struct LayerSurfaceSpec<'a> {
    namespace: &'a str,
    layer: Layer,
    anchor: Anchor,
    /// `None` sets no size, which requires anchoring opposite edges in
    /// both dimensions and lets the compositor assign it — what the
    /// overview does. Per the protocol, omitting a dimension without
    /// anchoring both its edges is a protocol error.
    size: Option<(u32, u32)>,
    exclusive_zone: i32,
    interactivity: KeyboardInteractivity,
    /// `None` leaves placement to the compositor.
    output: Option<&'a WlOutput>,
}

/// Make one layer-shell surface on explicit handles.
///
/// The whole construction sequence lives here: every shell surface —
/// panel, dock, banners, switcher, overview — is made through this, so a
/// protocol-level change has one edit site rather than five. Taking the
/// handles explicitly rather than reading `self.wayland` keeps it
/// callable without a live [`ShellHost`].
///
/// Ordering is immaterial: the protocol specifies layer, size, anchor,
/// exclusive zone, margin and interactivity as double-buffered state
/// applied together at `wl_surface.commit`.
fn make_layer_surface(
    compositor: &WlCompositor,
    layer_shell: &ZwlrLayerShellV1,
    qh: &QueueHandle<ShellHost>,
    spec: LayerSurfaceSpec<'_>,
) -> (WlSurface, ZwlrLayerSurfaceV1) {
    let surface = compositor.create_surface(qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &surface,
        spec.output,
        spec.layer,
        spec.namespace.to_owned(),
        qh,
        (),
    );
    if let Some((width, height)) = spec.size {
        layer_surface.set_size(width, height);
    }
    layer_surface.set_anchor(spec.anchor);
    layer_surface.set_exclusive_zone(spec.exclusive_zone);
    layer_surface.set_keyboard_interactivity(spec.interactivity);
    surface.commit();
    (surface, layer_surface)
}

/// Make one top-anchored panel layer surface on explicit handles,
/// bound to `output` (`None` leaves placement to the compositor).
/// Shared by startup creation and per-output reconcile so both paths
/// configure identically.
fn make_panel_surface(
    compositor: &WlCompositor,
    layer_shell: &ZwlrLayerShellV1,
    qh: &QueueHandle<ShellHost>,
    namespace: &str,
    height: u32,
    output: Option<&WlOutput>,
) -> (WlSurface, ZwlrLayerSurfaceV1) {
    make_layer_surface(
        compositor,
        layer_shell,
        qh,
        LayerSurfaceSpec {
            namespace,
            layer: Layer::Top,
            anchor: Anchor::Top | Anchor::Left | Anchor::Right,
            size: Some((0, height)),
            exclusive_zone: height as i32,
            interactivity: KeyboardInteractivity::OnDemand,
            output,
        },
    )
}

/// Make one bottom-anchored dock layer surface on explicit handles,
/// bound to `output`. Overlay layer with no exclusive zone, like the
/// switcher. Shared by startup creation and primary-anchor moves.
fn make_dock_surface(
    compositor: &WlCompositor,
    layer_shell: &ZwlrLayerShellV1,
    qh: &QueueHandle<ShellHost>,
    output: Option<&WlOutput>,
) -> (WlSurface, ZwlrLayerSurfaceV1) {
    make_layer_surface(
        compositor,
        layer_shell,
        qh,
        LayerSurfaceSpec {
            namespace: DOCK_NAMESPACE,
            layer: Layer::Overlay,
            anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
            size: Some((0, DOCK_H as u32)),
            exclusive_zone: 0,
            // OnDemand: the dock takes keyboard focus while its slots are
            // keyboard-driven, never stealing it otherwise.
            interactivity: KeyboardInteractivity::OnDemand,
            output,
        },
    )
}

/// One bound compositor output: the protocol object plus its
/// inventory name once `xdg_output` resolves it.
struct BoundOutput {
    /// Registry global name (for `GlobalRemove` matching).
    global: u32,
    /// Bound protocol object for surface creation.
    output: WlOutput,
    /// `xdg_output` tracker driving the name resolution.
    _xdg: Option<ZxdgOutputV1>,
    /// Inventory name from the `xdg_output` name event (`None` until
    /// it arrives).
    name: Option<String>,
}

/// Extra panel surface for one non-primary output: its own layer
/// surface, configured size, backing, and paint key, painted with
/// the same strip content as the primary. Presses and popups stay
/// on the primary surface.
struct ExtraPanel {
    /// Inventory name of the output this panel is bound to.
    name: String,
    /// Panel `wl_surface` for this output.
    surface: WlSurface,
    /// Top-anchored layer surface bound to this output.
    layer: ZwlrLayerSurfaceV1,
    /// Size last painted (repaint on configure resize).
    size: Option<(i32, i32)>,
    /// Buffer backing (pool fd must outlive the buffer).
    backing: Option<ShmBacking>,
    /// What the buffer currently shows; repaint on change.
    paint_key: Option<PanelPaintKey>,
}

/// Desired per-output surface set from the inventory plus the bound
/// `wl_output` names: panel outputs (primary first) and the dock
/// output. Pure planning — the tick applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputPlan {
    /// Output names needing a panel surface, primary first.
    panels: Vec<String>,
    /// Output name needing the dock surface (`None` while the
    /// primary's `wl_output` is unbound).
    dock: Option<String>,
}

/// Plan the per-output surfaces: every inventoried output whose
/// `wl_output` is bound gets a panel; the dock goes on the primary.
/// Unknown inventory (empty list, e.g. old compositor) plans
/// nothing — the legacy unbound surfaces stay live.
fn plan_output_surfaces(outputs: &[OutputInfo], bound: &[String]) -> OutputPlan {
    let panels: Vec<String> = outputs
        .iter()
        .filter(|info| bound.iter().any(|name| name == &info.name))
        .map(|info| info.name.clone())
        .collect();
    let dock = outputs
        .iter()
        .find(|info| info.primary)
        .map(|info| info.name.clone())
        .filter(|name| bound.iter().any(|bound| bound == name));
    OutputPlan { panels, dock }
}

/// Note-surfacing state per extension script: the last seen enabled
/// flag (fresh disables banner once) plus notice texts already shown.
#[derive(Debug, Default)]
struct ExtSeen {
    enabled: bool,
    notices: Vec<String>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct PaintKey {
    revision: Option<u64>,
    selected: Option<u64>,
    windows: usize,
    favorites: usize,
    active_workspace: u32,
    width: i32,
    height: i32,
    query: String,
    failure: Option<usize>,
    /// Keyboard focus cursor: any focus move repaints, so a stale
    /// result ring is impossible by construction.
    focus: Option<ShellFocus>,
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

/// Live banner layer surface and its configured size.
struct BannerSurface {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    backing: Option<ShmBacking>,
    width: i32,
    height: i32,
    /// Last requested height (re-request only on change, paint only at
    /// the configured size).
    requested_height: i32,
}

/// One visible banner's paint inputs: identity and chrome plus the
/// painted text, so an in-place replace repaints even when the id,
/// urgency, and expand flag all stay the same.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BannerKeyRow {
    id: u64,
    urgency: Urgency,
    expanded: bool,
    summary: String,
    body: String,
    has_actions: bool,
}

/// Repaint the banner strip when any of these change: visible banner
/// rows (identity, urgency, expand flag, and painted text), plus the
/// configured size.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BannerPaintKey {
    rows: Vec<BannerKeyRow>,
    width: i32,
    height: i32,
}

/// Visible banner paint rows behind [`BannerPaintKey`]: identity and
/// chrome plus the painted text, so an in-place replace repaints even
/// when the id, urgency, and expand flag all stay the same. Shared by
/// [`ShellHost::update_banners`] and the flood test, which pins the
/// row set bounded and stable under load.
fn banner_key_rows(center: &NotificationCenter) -> Vec<BannerKeyRow> {
    // The lock is uncontended on the tick path; a poisoned center
    // reads as no banners (same rule as the paint path below).
    center
        .banners()
        .iter()
        .map(|n| BannerKeyRow {
            id: n.id,
            urgency: n.urgency,
            expanded: n.expanded,
            summary: n.summary().to_owned(),
            body: n.body().to_owned(),
            has_actions: !n.pending_actions().is_empty(),
        })
        .collect()
}

/// Repaint the dock strip when any of these change: item order,
/// running windows, focus, pins, size, or the open stack grid.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DockPaintKey {
    items: Vec<(String, Vec<u64>, bool, bool)>,
    /// Script badges: name and text per contributing script.
    badges: Vec<(String, String)>,
    width: i32,
    height: i32,
    /// Open stack item plus its entry names (directory reads refresh
    /// the grid without reopening it).
    stack: Option<(usize, Vec<String>)>,
    /// Keyboard focus cursor: any focus move repaints, so a stale
    /// ring is impossible by construction.
    focus: Option<ShellFocus>,
}

/// Repaint the panel strip when any of these change.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PanelPaintKey {
    clock: String,
    /// Bar clock format: the calendar footer row names the flip, so
    /// a format change repaints even when the minute text coincides
    /// (noon reads `12:xx` either way).
    clock_format: ClockFormat,
    states: [(TileState, Option<u8>); 3],
    /// Queued unread notifications behind the bar presence marker;
    /// the strip repaints within one slow tick of queue changes.
    unread: usize,
    width: i32,
    height: i32,
    /// Open popup: calendar carries the day of month (midnight
    /// repaint), menus carry the tile index.
    popup: Option<(u8, u32)>,
    /// Hosted indicators: service, title, and menu labels.
    indicators: Vec<(String, String, Vec<String>)>,
    /// Script cells: name, text, and icon per contributing script.
    extensions: Vec<(String, String, String)>,
    /// Keyboard focus cursor: any focus move repaints, so a stale
    /// ring is impossible by construction.
    focus: Option<ShellFocus>,
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
    let fd = rustix::fs::memfd_create("roost-shm", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
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

/// Inline launch-failure state for one overview result row: the
/// pressed row the repaint marker paints on, plus the spawn error
/// text (kept for diagnostics and tests; the text row itself waits
/// for the toolkit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewFailure {
    /// Pressed result index the marker paints on.
    pub index: usize,
    /// Spawn error text, in [`HitError`] display form.
    pub message: String,
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
        let center = Arc::new(Mutex::new(NotificationCenter::new()));
        let notifications = NotificationBus::new(center.clone());
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
            center,
            banners: None,
            banner_key: None,
            panel_size: None,
            panel_backing: None,
            tiles: TileSet::system(),
            tiles_refreshed: None,
            panel_paint_key: None,
            seat: None,
            keyboard: None,
            pointer: None,
            pointer_pos: None,
            pointer_on_dock: false,
            pointer_on_banner: false,
            popup: PopupState::default(),
            dock_layer: None,
            dock_surface: None,
            dock_size: None,
            dock_backing: None,
            dock_paint_key: None,
            panel_output: None,
            dock_output: None,
            bound_outputs: Vec::new(),
            extra_panels: Vec::new(),
            xdg_manager: None,
            open_stack: None,
            stack_cache: Vec::new(),
            published_wallpaper: None,
            indicators: IndicatorHost::new(),
            extensions: ExtensionHost::new(extension_dir()),
            ext_seen: std::collections::HashMap::new(),
            pending_presses: Vec::new(),
            watcher: WatcherBus::new(),
            notifications,
            icon_cache: std::collections::HashMap::new(),
            icon_theme: crate::settings::DEFAULT_ICON_THEME.to_owned(),
            pending_dock: Vec::new(),
            pending_lock: false,
            xkb: None,
            search_text: String::new(),
            overview_failure: None,
            pending_submit: false,
            pending_dismiss: false,
            pending_nav: Vec::new(),
            pending_shell_submit: false,
            focus: None,
            return_stack: Vec::new(),
            overview_hits: Vec::new(),
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

    /// The host's notification center (the intake bus files through
    /// here; tests file directly). Shared with the bus edge, so wire
    /// arrivals show up in banners without a copy.
    pub fn notification_center(&self) -> Arc<Mutex<NotificationCenter>> {
        self.center.clone()
    }

    /// Load the persisted notification queue from `path` into the
    /// shared center, replacing whatever it holds. Missing, corrupt,
    /// or version-skewed files read as empty. The loaded path sticks
    /// to the center, so later mutations persist back to it.
    pub fn restore_notification_queue(&mut self, path: &Path) {
        if let Ok(mut center) = self.center.lock() {
            *center = NotificationCenter::load(path);
        }
    }

    /// Load the persisted queue from the system state file at host
    /// start. Same fail-closed rule as
    /// [`ShellHost::restore_notification_queue`]: a bad file never
    /// blocks the panel.
    pub fn load_notification_queue(&mut self) {
        self.restore_notification_queue(&NotificationCenter::system_path());
    }

    /// Load the Roost-owned prefs from the system state file at host
    /// start. Same fail-closed rule as the notification queue: a bad
    /// file never blocks the panel.
    pub fn load_roost_prefs(&mut self) {
        self.tiles.load_prefs_system();
    }

    /// Tear down the banner surface, if any (same explicit-destroy
    /// rule as the overview and switcher).
    fn destroy_banners(&mut self) {
        if let Some(banners) = self.banners.take() {
            banners.layer.destroy();
            banners.surface.destroy();
        }
        self.banner_key = None;
    }

    /// Reconcile the banner surface with the notification center (call
    /// after every center mutation and per loop tick): create the
    /// bottom-right strip while banners queue, destroy it when the
    /// queue drains, repaint when the visible rows or size change.
    /// Pure no-op without attached Wayland globals.
    fn update_banners(&mut self) {
        let rows: Vec<BannerKeyRow> = self
            .center
            .lock()
            .map(|center| banner_key_rows(&center))
            .unwrap_or_default();
        if rows.is_empty() {
            self.destroy_banners();
            return;
        }
        let Some(wayland) = self.wayland.clone() else {
            return;
        };
        if self.banners.is_none() {
            let expanded: Vec<bool> = rows.iter().map(|row| row.expanded).collect();
            let want = banner_strip_height(&expanded);
            let (surface, layer) = make_layer_surface(
                &wayland.compositor,
                &wayland.layer_shell,
                &wayland.qh,
                LayerSurfaceSpec {
                    namespace: BANNER_NAMESPACE,
                    layer: Layer::Overlay,
                    anchor: Anchor::Bottom | Anchor::Right,
                    size: Some((BANNER_STRIP_W as u32, want as u32)),
                    exclusive_zone: 0,
                    interactivity: KeyboardInteractivity::None,
                    output: None,
                },
            );
            self.banners = Some(BannerSurface {
                surface,
                layer,
                backing: None,
                width: 0,
                height: 0,
                requested_height: want,
            });
            self.banner_key = None;
        }
        let banners = self.banners.as_mut().expect("created above");
        if banners.width <= 0 || banners.height <= 0 {
            return;
        }
        // The row set changed height (expand toggles, queue growth):
        // re-request once; the configure round-trip repaints at the
        // new size.
        let expanded: Vec<bool> = rows.iter().map(|row| row.expanded).collect();
        let want = banner_strip_height(&expanded);
        if want != banners.requested_height {
            banners.layer.set_size(BANNER_STRIP_W as u32, want as u32);
            banners.surface.commit();
            banners.requested_height = want;
            return;
        }
        let key = BannerPaintKey {
            rows: rows.clone(),
            width: banners.width,
            height: banners.height,
        };
        if self.banner_key.as_ref() == Some(&key) {
            return;
        }
        let mut canvas = BannerCanvas::new(banners.width, banners.height);
        let cells: Vec<BannerRow<'_>> = rows
            .iter()
            .map(|row| BannerRow {
                summary: &row.summary,
                body: &row.body,
                urgency: row.urgency,
                expanded: row.expanded,
                has_actions: row.has_actions,
            })
            .collect();
        canvas.render(&cells);
        if let Some(backing) = shm_upload(
            &wayland.shm,
            &wayland.qh,
            canvas.pixels(),
            banners.width,
            banners.height,
        ) {
            banners.surface.attach(Some(&backing.buffer), 0, 0);
            banners.surface.damage(0, 0, banners.width, banners.height);
            banners.surface.commit();
            banners.backing = Some(backing);
            self.banner_key = Some(key);
        }
    }

    /// Press at banner-surface coordinates: the dismiss box closes
    /// the banner, anywhere else on the row invokes its `default`
    /// action (falling back to dismiss when the banner offers no
    /// default key). Routes through [`NotificationBus`] so the app
    /// hears `ActionInvoked` / `NotificationClosed`, then reconciles
    /// the strip. Never touches window selection or keyboard focus:
    /// the banner layer keeps `KeyboardInteractivity::None` and this
    /// path owns no selection, opens nothing, and edits no text.
    pub fn press_banner(&mut self, x: i32, y: i32) {
        let rows: Vec<(u64, bool, bool)> = self
            .center
            .lock()
            .map(|center| {
                center
                    .banners()
                    .iter()
                    .map(|n| (n.id, n.expanded, n.pending_actions().contains(&"default")))
                    .collect()
            })
            .unwrap_or_default();
        if rows.is_empty() {
            return;
        }
        // The strip is always requested at the fixed width, so
        // headless presses hit-test against the same geometry the
        // canvas paints.
        let expanded: Vec<bool> = rows.iter().map(|(_, expanded, _)| *expanded).collect();
        let (index, dismiss) = match banner_hit(&expanded, BANNER_STRIP_W, x, y) {
            BannerHit::ActionRow(index) => (index, !rows[index].2),
            BannerHit::Dismiss(index) => (index, true),
            BannerHit::Miss => return,
        };
        let id = rows[index].0 as u32;
        if dismiss {
            // Reason 2: dismissed by the user (freedesktop close reasons).
            let _ = self.notifications.dismiss_banner(id, 2);
        } else {
            let _ = self.notifications.invoke_action(id, "default");
        }
        self.update_banners();
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
            let (surface, layer) = make_layer_surface(
                &wayland.compositor,
                &wayland.layer_shell,
                &wayland.qh,
                LayerSurfaceSpec {
                    namespace: SWITCHER_NAMESPACE,
                    layer: Layer::Overlay,
                    anchor: Anchor::Bottom | Anchor::Left | Anchor::Right,
                    size: Some((0, SWITCHER_STRIP_H as u32)),
                    exclusive_zone: 0,
                    interactivity: KeyboardInteractivity::None,
                    output: None,
                },
            );
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

    /// Paint key for one overview size: revision, selection,
    /// windows, favorites, workspace, query, failure marker, and
    /// the keyboard focus cursor, so any content or focus change
    /// repaints.
    fn overview_render_key(&self, revision: Option<u64>, width: i32, height: i32) -> PaintKey {
        PaintKey {
            revision,
            selected: self.model.selected(),
            windows: self.model.windows().len(),
            favorites: self.favorites.ids().len(),
            active_workspace: self.model.active_workspace(),
            width,
            height,
            query: self.search_text.clone(),
            failure: self.overview_failure.as_ref().map(|f| f.index),
            focus: self.focus,
        }
    }

    /// One surface-update pass for every run loop: overview, switcher,
    /// panel status, and banners. Both the control-paced loop and the
    /// pure-panel fallback loop run this, so no mode can open a surface
    /// without painting it (regression: the fallback loop only
    /// refreshed panel status, leaving the overview blank forever).
    fn update_shell_surfaces(&mut self, revision: Option<u64>) {
        self.update_overview(revision);
        self.update_switcher();
        self.update_panel_status();
        self.update_banners();
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
            let (surface, layer) = make_layer_surface(
                &wayland.compositor,
                &wayland.layer_shell,
                &wayland.qh,
                LayerSurfaceSpec {
                    namespace: OVERVIEW_NAMESPACE,
                    layer: Layer::Overlay,
                    anchor: Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right,
                    // No size: anchored on all four edges, so the
                    // compositor assigns it in the configure event.
                    size: None,
                    exclusive_zone: 0,
                    // The overview takes typed search: declare it so the
                    // compositor parks keyboard focus here while open.
                    interactivity: KeyboardInteractivity::Exclusive,
                    output: None,
                },
            );
            self.overview = Some(OverviewSurface {
                surface,
                layer,
                backing: None,
                width: 0,
                height: 0,
            });
            self.paint_key = None;
        }
        // Result row behind the ring, decided before the surface
        // borrow: refreshing here keeps paint and activation on
        // the same rows.
        let result_ring = match self.focus {
            Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor,
            }) => {
                self.refresh_overview_hits();
                (cursor < self.overview_hits.len()).then_some(cursor)
            }
            _ => None,
        };
        let Some(size) = self
            .overview
            .as_ref()
            .filter(|overview| overview.width > 0 && overview.height > 0)
            .map(|overview| (overview.width, overview.height))
        else {
            return;
        };
        let (width, height) = size;
        let key = self.overview_render_key(revision, width, height);
        if self.paint_key.as_ref() == Some(&key) {
            return;
        }
        let mut canvas = OverviewCanvas::new(width, height);
        canvas.render(&self.model, self.favorites.ids().len());
        canvas.draw_query(&self.search_text);
        if let Some(failure) = &self.overview_failure {
            canvas.draw_launch_failure(failure.index);
        }
        // The ring paints over the grid, so the row cursor reads
        // wherever the selection border was.
        if let Some(cursor) = result_ring {
            canvas.draw_result_focus(cursor);
        }
        if let Some(backing) = shm_upload(&wayland.shm, &wayland.qh, canvas.pixels(), width, height)
        {
            let overview = self.overview.as_mut().expect("sized above");
            overview.surface.attach(Some(&backing.buffer), 0, 0);
            overview.surface.damage(0, 0, width, height);
            overview.surface.commit();
            // The old backing drops here, after the new buffer is
            // committed — the compositor never reads a freed mapping.
            overview.backing = Some(backing);
            self.paint_key = Some(key);
        }
    }

    /// Paint the panel strip into a fresh shm buffer and attach it.
    /// Repaints when the size, clock minute, tile states, unread
    /// count, or popup change; called on configure and from the slow
    /// status tick. `height` is the arranged surface height:
    /// strip-only, or strip plus the popup band while a popup is open.
    /// Paint key for one panel strip size: clock, tiles, unread,
    /// indicators, and popup state, so any content change repaints.
    fn panel_render_key(&self, width: i32, height: i32) -> PanelPaintKey {
        let tiles = self.tiles.tiles();
        let today = jiff::Zoned::now().date();
        let popup = self.popup.body().map(|body| match body {
            PopupBody::Calendar => (0u8, today.day() as u32),
            PopupBody::Menu(index) => (1u8, index as u32),
            PopupBody::IndicatorMenu(index) => (2u8, index as u32),
        });
        let unread = self
            .center
            .lock()
            .map(|center| center.unread_count())
            .unwrap_or(0);
        PanelPaintKey {
            clock: self.tiles.clock.clone(),
            clock_format: self.tiles.settings.clock_format,
            states: tiles.map(|tile| (tile.state, tile.level)),
            unread,
            width,
            height,
            focus: self.focus,
            popup,
            indicators: self
                .indicators
                .items()
                .iter()
                .map(|item| {
                    (
                        item.service.clone(),
                        item.title.clone(),
                        item.menu.iter().map(|row| row.label.clone()).collect(),
                    )
                })
                .collect(),
            extensions: self
                .extensions
                .states()
                .iter()
                .filter(|state| state.enabled)
                .filter_map(|state| {
                    state.output.cell_text.clone().map(|text| {
                        (
                            state.name.clone(),
                            text,
                            state.output.cell_icon.clone().unwrap_or_default(),
                        )
                    })
                })
                .collect(),
        }
    }

    /// Pixel content for one panel strip: the status strip plus the
    /// popup band when one is open and tall enough. Shared by the
    /// primary surface and every extra panel, so all outputs show
    /// the same strip.
    ///
    /// Script cells paint in their own pass left of the indicator
    /// row: a frame plus the cell text's initial, mirroring the
    /// indicator look. Extension engines never run here — only
    /// cached outputs paint.
    fn render_panel_pixels(&self, width: i32, height: i32) -> Vec<u8> {
        let strip_h = self.panel.height as i32;
        let today = jiff::Zoned::now().date();
        let unread = self
            .center
            .lock()
            .map(|center| center.unread_count())
            .unwrap_or(0);
        let mut pixels = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL];
        paint_panel(
            &mut pixels,
            width,
            height,
            strip_h,
            &self.tiles.clock,
            &[self.tiles.network, self.tiles.power, self.tiles.sound],
            unread,
        );
        if let Some(body) = self.popup.body() {
            if height > strip_h {
                let layout = panel_layout(width, strip_h, &self.tiles.clock);
                let tiles = [self.tiles.network, self.tiles.power, self.tiles.sound];
                // Toggle rows paint from the cached radio and mixer
                // states, never the wire: the slow tick owns every
                // D-Bus read.
                let radio = self.tiles.radio_state();
                let rows = network_rows(
                    radio.map(|state| state.wireless),
                    radio.map(|state| state.networking),
                    self.tiles.network_pending().is_some(),
                );
                let output = self.tiles.sound_state();
                let sound = sound_rows(
                    output.map(|state| state.muted),
                    output.map(|state| state.volume),
                    self.tiles.sound_pending().is_some(),
                );
                paint_popup(
                    &mut pixels,
                    width,
                    &layout,
                    body,
                    today,
                    &tiles,
                    self.indicators.items(),
                    &rows,
                    &sound,
                    &lock_rows(),
                    self.tiles.settings.clock_format,
                    self.tiles.prefs.clock_show_weekday,
                );
            }
        }
        // Indicator cells sit left of the sound tile, painted over
        // the strip in their own pass.
        let indicator_count = self.indicators.items().len();
        let cells = indicator_cells(indicator_right_x(width), strip_h, indicator_count);
        if !cells.is_empty() {
            paint_indicators(&mut pixels, width, &cells, self.indicators.items());
        }
        // Script cells continue the row left of the indicators.
        let script = self.extension_cells();
        if !script.is_empty() {
            let right = indicator_right_x(width) - indicator_count as i32 * INDICATOR_CELL;
            let rects = indicator_cells(right, strip_h, script.len());
            let texts: Vec<&str> = script.iter().map(|(_, text, _)| text.as_str()).collect();
            paint_script_cells(&mut pixels, width, &rects, &texts);
        }
        // Keyboard focus ring paints last, over strip and
        // indicators alike, so the focused stop always reads.
        if let Some(ShellFocus {
            region: FocusRegion::Panel,
            cursor,
        }) = self.focus
        {
            let layout = panel_layout(width, strip_h, &self.tiles.clock);
            let stop = if cursor == 0 {
                Some(layout.clock)
            } else if cursor < PANEL_STOPS {
                layout.tiles.get(cursor - 1).copied()
            } else {
                let cells = indicator_cells(
                    indicator_right_x(width),
                    strip_h,
                    self.indicators.items().len(),
                );
                cells.get(cursor - PANEL_STOPS).copied()
            };
            if let Some(rect) = stop {
                paint_focus_ring(&mut pixels, width, &rect);
            }
        }
        // Popup row rings paint over the band, so the row cursor
        // reads wherever the strip ring was.
        if height > strip_h {
            if let Some(rect) = self.focused_popup_row(width, strip_h) {
                paint_focus_ring(&mut pixels, width, &rect);
            }
        }
        pixels
    }

    /// Paint box for the focused popup row, if the cursor names a
    /// live row of the open popup: calendar footers reuse their
    /// press rects, menu rows invert the press hit-test.
    fn focused_popup_row(&self, width: i32, strip_h: i32) -> Option<Rect> {
        let Some(ShellFocus {
            region: FocusRegion::Popup,
            cursor,
        }) = self.focus
        else {
            return None;
        };
        let body = self.popup.body()?;
        if cursor >= self.popup_row_count() {
            return None;
        }
        let layout = panel_layout(width, strip_h, &self.tiles.clock);
        let open_box = popup_box(&layout, body);
        match body {
            PopupBody::Calendar => Some(if cursor == 0 {
                calendar_clock_row(&open_box)
            } else {
                calendar_weekday_row(&open_box)
            }),
            PopupBody::Menu(_) => tile_row_rect(&open_box, cursor, self.popup_row_count()),
            PopupBody::IndicatorMenu(index) => {
                let count = self
                    .indicators
                    .items()
                    .get(index)
                    .map(|item| item.menu.len())
                    .unwrap_or(0);
                menu_row_rect(&open_box, cursor, count)
            }
        }
    }

    fn paint_panel_surface(&mut self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        let key = self.panel_render_key(width, height);
        if self.panel_size == Some((width, height))
            && self.panel_backing.is_some()
            && self.panel_paint_key.as_ref() == Some(&key)
        {
            return;
        }
        let Some(wayland) = self.wayland.clone() else {
            // No compositor attached (unit tests): record the key so
            // the slow-tick repaint decision stays observable. The shm
            // upload below still needs Wayland, and `panel_backing`
            // stays `None`, so a later attach repaints for real.
            self.panel_paint_key = Some(key);
            return;
        };
        let pixels = self.render_panel_pixels(width, height);
        let Some(surface) = self.surface.as_ref() else {
            return;
        };
        if let Some(backing) = shm_upload(&wayland.shm, &wayland.qh, &pixels, width, height) {
            surface.attach(Some(&backing.buffer), 0, 0);
            surface.damage(0, 0, width, height);
            surface.commit();
            self.panel_backing = Some(backing);
            self.panel_size = Some((width, height));
            self.panel_paint_key = Some(key);
        }
    }

    /// Paint one extra (non-primary) panel at its configured size,
    /// with the same strip content as the primary. Own backing, size,
    /// and key: each output repaints independently on its configure.
    fn paint_extra_panel(&mut self, index: usize, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        let key = self.panel_render_key(width, height);
        let same = self.extra_panels.get(index).is_some_and(|extra| {
            extra.size == Some((width, height)) && extra.paint_key.as_ref() == Some(&key)
        });
        if same && self.extra_panels[index].backing.is_some() {
            return;
        }
        let Some(wayland) = self.wayland.clone() else {
            if let Some(extra) = self.extra_panels.get_mut(index) {
                extra.paint_key = Some(key);
            }
            return;
        };
        let pixels = self.render_panel_pixels(width, height);
        let Some(extra) = self.extra_panels.get(index) else {
            return;
        };
        if let Some(backing) = shm_upload(&wayland.shm, &wayland.qh, &pixels, width, height) {
            extra.surface.attach(Some(&backing.buffer), 0, 0);
            extra.surface.damage(0, 0, width, height);
            extra.surface.commit();
            let extra = &mut self.extra_panels[index];
            extra.backing = Some(backing);
            extra.size = Some((width, height));
            extra.paint_key = Some(key);
        }
    }

    /// Enabled scripts contributing a bar cell, in load order: script
    /// name, cell text, and press action id.
    fn extension_cells(&self) -> Vec<(String, String, Option<String>)> {
        self.extensions
            .states()
            .iter()
            .filter(|state| state.enabled)
            .filter_map(|state| {
                state
                    .output
                    .cell_text
                    .clone()
                    .map(|text| (state.name.clone(), text, state.output.press_id.clone()))
            })
            .collect()
    }

    /// Enabled scripts contributing a dock badge, in load order:
    /// script name and badge text.
    fn extension_badges(&self) -> Vec<(String, String)> {
        self.extensions
            .states()
            .iter()
            .filter(|state| state.enabled)
            .filter_map(|state| {
                state
                    .output
                    .badge
                    .clone()
                    .map(|badge| (state.name.clone(), badge))
            })
            .collect()
    }

    /// Drain queued strip presses into the declaring scripts'
    /// handlers. Runs on the slow tick only, so the press event path
    /// itself never executes script code.
    fn drain_extension_presses(&mut self) {
        for id in std::mem::take(&mut self.pending_presses) {
            self.extensions.press(&id);
        }
    }

    /// Banner fresh script notes exactly once each: explicit notices
    /// show on first sight, disable notes on the enabled-to-disabled
    /// transition. Seen texts are capped; pathological chatter eventually
    /// re-banners rather than growing memory without bound.
    fn surface_extension_notes(&mut self) {
        let mut fresh = Vec::new();
        for state in self.extensions.states() {
            let seen = self.ext_seen.entry(state.name.clone()).or_default();
            if !state.enabled && seen.enabled {
                if let Some(note) = state.note.clone() {
                    fresh.push((state.name.clone(), note));
                }
            }
            seen.enabled = state.enabled;
            for notice in &state.output.notices {
                if !seen.notices.contains(notice) {
                    fresh.push((state.name.clone(), notice.clone()));
                    seen.notices.push(notice.clone());
                }
            }
            if seen.notices.len() > 64 {
                seen.notices.drain(..seen.notices.len() - 64);
            }
        }
        for (name, body) in fresh {
            if let Ok(mut center) = self.center.lock() {
                center.notify(
                    "extensions",
                    &name,
                    &body,
                    Vec::<NotificationAction>::new(),
                    Urgency::Normal,
                    None,
                );
            }
        }
    }

    /// Slow-tick status refresh for the run loop: re-probe services at
    /// most every two seconds, repainting the strip when the clock
    /// minute or any tile changed. Pure no-op without attached
    /// Wayland globals.
    fn update_panel_status(&mut self) {
        let now = std::time::Instant::now();
        let due = self
            .tiles_refreshed
            .is_none_or(|last| now.duration_since(last) >= std::time::Duration::from_secs(2));
        if due {
            self.tiles.ensure_radio();
            self.tiles.ensure_sound();
            self.tiles.refresh();
            self.tiles_refreshed = Some(now);
            self.publish_wallpaper();
            self.sync_icon_theme();
            self.poll_indicators();
            self.poll_notifications();
            // Extension scripts run here, off the paint and event
            // paths: the drive reloads changed scripts and refreshes
            // cached outputs, queued presses drain into handlers,
            // then fresh notes banner once each.
            self.extensions.drive();
            self.drain_extension_presses();
            self.surface_extension_notes();
        }
        if let Some((width, height)) = self.panel_size {
            self.paint_panel_surface(width, height);
        }
        if let Some((width, height)) = self.dock_size {
            self.paint_dock_surface(width, height);
        }
        // Extra panels repaint on their own sizes when content
        // changed; the paint keys skip the steady state.
        let extra_sizes: Vec<(usize, i32, i32)> = self
            .extra_panels
            .iter()
            .enumerate()
            .filter_map(|(index, extra)| extra.size.map(|(width, height)| (index, width, height)))
            .collect();
        for (index, width, height) in extra_sizes {
            self.paint_extra_panel(index, width, height);
        }
    }

    /// Surface height for the current popup state: strip-only, or
    /// strip plus the popup band while a popup is open. The exclusive
    /// zone stays at strip height either way so windows only ever
    /// clear the strip.
    fn popup_surface_height(&self) -> i32 {
        self.panel.height as i32
            + if self.popup.is_open() {
                POPUP_HEIGHT
            } else {
                0
            }
    }

    /// Apply the popup state to the live surface: request the matching
    /// size and repaint at it. The compositor's configure round trip
    /// repaints again at the arranged size; without Wayland attached
    /// (tests) this only flips state.
    fn apply_popup_size(&mut self) {
        let height = self.popup_surface_height();
        if let (Some(layer), Some(surface)) = (self.layer_surface.as_ref(), self.surface.as_ref()) {
            layer.set_size(0, height as u32);
            surface.commit();
        }
        if let Some((width, _)) = self.panel_size {
            self.panel_paint_key = None;
            self.paint_panel_surface(width, height);
        }
    }

    /// Close the open popup, if any, shrinking the surface back.
    /// Settling restores the parked cursor (or clears a stranded
    /// one), so focus never sticks on the closed surface.
    /// No-op (beyond state) without Wayland attached.
    pub fn close_popup(&mut self) {
        if !self.popup.is_open() {
            return;
        }
        self.popup.dismiss();
        self.apply_popup_size();
        self.settle_focus();
    }

    /// Left press at panel-surface coordinates: toggle the calendar on
    /// the clock, a menu on a tile, dismiss anywhere else. Resizes and
    /// repaints the surface to match. Ignored before the first
    /// configure (no arranged size to hit-test against yet).
    pub fn press_panel(&mut self, x: i32, y: i32) {
        let Some((width, _)) = self.panel_size else {
            return;
        };
        let strip_h = self.panel.height as i32;
        let layout = panel_layout(width, strip_h, &self.tiles.clock);
        // Indicator cells toggle their menus before the strip
        // layout sees the press.
        let cells = indicator_cells(
            indicator_right_x(width),
            strip_h,
            self.indicators.items().len(),
        );
        if let Some(hit) = indicator_at(&cells, x, y) {
            if self.popup.body() == Some(PopupBody::IndicatorMenu(hit)) {
                self.popup.dismiss();
            } else {
                self.open_indicator_menu(hit);
            }
            self.apply_popup_size();
            return;
        }
        // Script cells queue their press actions before the strip
        // layout sees the press; cells without an action are inert.
        // Queued ids drain into handlers on the slow tick, keeping
        // script code off the event path.
        let script = self.extension_cells();
        if !script.is_empty() {
            let right =
                indicator_right_x(width) - self.indicators.items().len() as i32 * INDICATOR_CELL;
            let rects = indicator_cells(right, strip_h, script.len());
            if let Some(hit) = indicator_at(&rects, x, y) {
                if let Some(id) = script[hit].2.clone() {
                    self.pending_presses.push(id);
                }
                return;
            }
        }
        // Row presses inside an open indicator menu fire, then
        // dismiss; anything else falls through to the strip.
        if let Some(PopupBody::IndicatorMenu(index)) = self.popup.body() {
            let open_box = popup_box(&layout, PopupBody::IndicatorMenu(index));
            if open_box.contains(x, y) {
                self.fire_indicator_row(index, y, &open_box);
                self.apply_popup_size();
                return;
            }
        }
        // Row presses inside the open calendar fire the footer
        // toggles (clock format above, Roost prefs below); the
        // calendar stays open on the new face, and anything else
        // falls through to the strip.
        if self.popup.body() == Some(PopupBody::Calendar) {
            let open_box = popup_box(&layout, PopupBody::Calendar);
            if open_box.contains(x, y) {
                self.fire_clock_row(&open_box, x, y);
                self.fire_weekday_row(&open_box, x, y);
                self.apply_popup_size();
                return;
            }
        }
        // Row presses inside the open network menu fire the radio
        // toggle; the menu stays open on the pending face, and anything
        // else falls through to the strip.
        if let Some(PopupBody::Menu(index)) = self.popup.body() {
            if index == NETWORK_TILE_INDEX {
                let open_box = popup_box(&layout, PopupBody::Menu(index));
                if open_box.contains(x, y) {
                    self.fire_network_row(&open_box, y);
                    self.apply_popup_size();
                    return;
                }
            }
            // Row presses inside the open sound menu fire mute or
            // volume; same pending-face shape as the network menu.
            if index == SOUND_TILE_INDEX {
                let open_box = popup_box(&layout, PopupBody::Menu(index));
                if open_box.contains(x, y) {
                    self.fire_sound_row(&open_box, y);
                    self.apply_popup_size();
                    return;
                }
            }
            // Row presses inside the open power menu arm a manual
            // lock request; the run loop's driver sends it, and the
            // lock screen engages when the locked snapshot lands.
            if index == POWER_TILE_INDEX {
                let open_box = popup_box(&layout, PopupBody::Menu(index));
                if open_box.contains(x, y) {
                    self.fire_lock_row(&open_box, y);
                    self.apply_popup_size();
                    return;
                }
            }
        }
        let open_box = self.popup.body().map(|body| popup_box(&layout, body));
        self.popup.press(&layout, open_box, x, y);
        self.apply_popup_size();
        // A press may have dismissed the popup under a parked
        // keyboard cursor: settle it instead of stranding it.
        self.settle_focus();
    }

    /// Fire the calendar footer row under the popup point: flips the
    /// bar clock between twelve- and twenty-four-hour. Presses
    /// elsewhere in the calendar keep it open; a refused write keeps
    /// the last good snapshot quietly and the calendar stays open.
    fn fire_clock_row(&mut self, open_box: &crate::popup::Rect, x: i32, y: i32) {
        if !calendar_clock_row(open_box).contains(x, y) {
            return;
        }
        self.fire_calendar_index(0);
    }

    /// Fire the calendar prefs row under the popup point: flips the
    /// bar-clock weekday prefix. Presses elsewhere in the calendar
    /// keep it open; the calendar stays open on the new face.
    fn fire_weekday_row(&mut self, open_box: &crate::popup::Rect, x: i32, y: i32) {
        if !calendar_weekday_row(open_box).contains(x, y) {
            return;
        }
        self.fire_calendar_index(1);
    }

    /// Fire calendar footer row `row` by index: row 0 flips the bar
    /// clock format, row 1 flips the weekday prefix. The keyboard
    /// twin of the footer press path, sharing both toggles; the
    /// calendar stays open on the new face either way.
    fn fire_calendar_index(&mut self, row: usize) {
        match row {
            0 => {
                self.toggle_clock_format();
            }
            1 => {
                self.toggle_clock_weekday();
            }
            _ => {}
        }
    }

    /// Flip the bar-clock weekday prefix from the calendar prefs row:
    /// update the snapshot at once and persist to the prefs file,
    /// repaint on the tick.
    pub fn toggle_clock_weekday(&mut self) -> bool {
        self.tiles.toggle_clock_show_weekday()
    }

    /// Flip the bar clock between twelve- and twenty-four-hour from
    /// the calendar toggle row: write the shared key, re-read the
    /// snapshot at once, repaint on the tick. A refused write keeps
    /// the last good snapshot, quietly.
    pub fn toggle_clock_format(&mut self) -> bool {
        self.toggle_clock_format_with(&crate::settings::GioBackend)
    }

    /// [`toggle_clock_format`](Self::toggle_clock_format) against an
    /// explicit backend (tests substitute a fake; the press path
    /// passes the platform backend).
    pub fn toggle_clock_format_with(
        &mut self,
        backend: &dyn crate::settings::SettingsBackend,
    ) -> bool {
        self.tiles.toggle_clock_format_with(backend)
    }

    /// Set the desktop wallpaper from the shell picker surface: write
    /// the shared key, re-read the snapshot at once, and publish to
    /// the compositor drop file on this same call (the slow tick
    /// would republish within two seconds; the picker must not wait
    /// for it). A refused write keeps the last good snapshot,
    /// quietly, and publishes nothing new.
    pub fn set_wallpaper_uri(&mut self, uri: &str) -> bool {
        self.set_wallpaper_uri_with(uri, &crate::settings::GioBackend)
    }

    /// [`set_wallpaper_uri`](Self::set_wallpaper_uri) against an
    /// explicit backend (tests substitute a fake; the picker path
    /// passes the platform backend).
    pub fn set_wallpaper_uri_with(
        &mut self,
        uri: &str,
        backend: &dyn crate::settings::SettingsBackend,
    ) -> bool {
        let done = self.tiles.set_wallpaper_uri_with(uri, backend);
        self.publish_wallpaper();
        done
    }

    /// Fire the network menu row under the popup point: row 0 toggles
    /// wifi, row 1 toggles networking. Disabled rows (pending toggle
    /// or unknown radio state) never fire; a refused write reports
    /// why on stderr and the menu stays open.
    fn fire_network_row(&mut self, open_box: &crate::popup::Rect, y: i32) {
        let rows = network_rows(
            self.tiles.radio_state().map(|state| state.wireless),
            self.tiles.radio_state().map(|state| state.networking),
            self.tiles.network_pending().is_some(),
        );
        let Some(row) = tile_row_at(open_box, y, rows.len()) else {
            return;
        };
        self.fire_network_index(row);
    }

    /// Fire network menu row `row` by index: the keyboard twin of
    /// [`fire_network_row`](Self::fire_network_row), sharing its
    /// enabled gating and toggle calls.
    fn fire_network_index(&mut self, row: usize) {
        let radio = self.tiles.radio_state();
        let rows = network_rows(
            radio.map(|state| state.wireless),
            radio.map(|state| state.networking),
            self.tiles.network_pending().is_some(),
        );
        let Some(row_state) = rows.get(row) else {
            return;
        };
        if !row_state.enabled {
            return;
        }
        match row {
            0 => {
                self.tiles
                    .set_wifi_enabled(!radio.map(|state| state.wireless).unwrap_or(true));
            }
            1 => {
                self.tiles
                    .set_networking_enabled(!radio.map(|state| state.networking).unwrap_or(true));
            }
            _ => {}
        }
    }

    /// Fire the sound menu row under the popup point: row 0 toggles
    /// mute, row 1 nudges volume down, row 2 nudges volume up.
    /// Disabled rows (pending toggle, unknown output state, or a
    /// volume bound) never fire; a refused write reports why on
    /// stderr and the menu stays open.
    fn fire_sound_row(&mut self, open_box: &crate::popup::Rect, y: i32) {
        let rows = sound_rows(
            self.tiles.sound_state().map(|state| state.muted),
            self.tiles.sound_state().map(|state| state.volume),
            self.tiles.sound_pending().is_some(),
        );
        let Some(row) = tile_row_at(open_box, y, rows.len()) else {
            return;
        };
        self.fire_sound_index(row);
    }

    /// Fire sound menu row `row` by index: the keyboard twin of
    /// [`fire_sound_row`](Self::fire_sound_row), sharing its
    /// enabled gating and mixer calls.
    fn fire_sound_index(&mut self, row: usize) {
        let output = self.tiles.sound_state();
        let rows = sound_rows(
            output.map(|state| state.muted),
            output.map(|state| state.volume),
            self.tiles.sound_pending().is_some(),
        );
        let Some(row_state) = rows.get(row) else {
            return;
        };
        if !row_state.enabled {
            return;
        }
        match row {
            0 => {
                self.tiles
                    .set_muted(!output.map(|state| state.muted).unwrap_or(false));
            }
            1 => {
                self.tiles.volume_down();
            }
            2 => {
                self.tiles.volume_up();
            }
            _ => {}
        }
    }

    /// Fire the power menu row under the popup point: the single
    /// lock row arms a manual lock request the run loop's driver
    /// sends; the menu stays open until the lock screen engages.
    fn fire_lock_row(&mut self, open_box: &crate::popup::Rect, y: i32) {
        let rows = lock_rows();
        let Some(row) = tile_row_at(open_box, y, rows.len()) else {
            return;
        };
        self.fire_lock_index(row);
    }

    /// Fire power menu row `row` by index: the keyboard twin of
    /// [`fire_lock_row`](Self::fire_lock_row), sharing its enabled
    /// gating and lock arming.
    fn fire_lock_index(&mut self, row: usize) {
        let rows = lock_rows();
        let Some(row_state) = rows.get(row) else {
            return;
        };
        if !row_state.enabled {
            return;
        }
        self.pending_lock = true;
    }

    /// Take an armed manual lock request for the run loop's driver.
    pub fn take_lock(&mut self) -> bool {
        std::mem::take(&mut self.pending_lock)
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

    /// Attach the seat keyboard path (the run functions call this
    /// after binding; tests leave keyboard empty and every key op
    /// below no-ops without it).
    pub fn attach_seat(&mut self, seat: WlSeat, qh: &QueueHandle<ShellHost>) {
        let keyboard = seat.get_keyboard(qh, ());
        self.seat = Some(seat);
        self.keyboard = Some(keyboard);
    }

    /// Live overview search text (what the next paint shows).
    pub fn search_text(&self) -> &str {
        &self.search_text
    }

    /// Consume a pending Enter activation, if any.
    pub fn take_submit(&mut self) -> bool {
        std::mem::take(&mut self.pending_submit)
    }

    /// Consume a pending Escape dismissal, if any.
    pub fn take_dismiss(&mut self) -> bool {
        std::mem::take(&mut self.pending_dismiss)
    }

    /// Feed one server key event (evdev code) into overview search.
    /// Text edits re-query the hub; submit/dismiss arm the flags the
    /// run loop consumes with a control client. Everything applies
    /// only while the overview is open: keys arriving otherwise (the
    /// compositor parked focus elsewhere) must not type into a
    /// hidden box or fire activations.
    pub fn on_key(&mut self, key: u32, pressed: bool) {
        let Some(feed) = self.xkb.as_mut() else {
            return;
        };
        let action = feed.key(key, pressed);
        // Navigation actions queue in both states: the strip is
        // visible with the overview closed, and the focus model
        // (task 2) consumes the queue. Search actions below keep
        // their overview-open gating byte-identical.
        if matches!(
            action,
            KeyAction::Up
                | KeyAction::Down
                | KeyAction::Left
                | KeyAction::Right
                | KeyAction::Next
                | KeyAction::Previous
                | KeyAction::CycleRegion
                | KeyAction::FocusDock
                | KeyAction::FocusPanel
        ) {
            if pressed {
                self.pending_nav.push(action);
            }
            return;
        }
        if !self.model.is_overview_open() {
            // The overview owns keys while open; a popup still answers
            // Escape when nothing else does, and Enter activates the
            // focused shell stop. Submit stays disarmed here so
            // `take_submit` keeps its overview-open-only contract.
            if pressed {
                match action {
                    KeyAction::Dismiss => {
                        // Split Escape: an open popup closes and
                        // hands focus back to the parked cursor; with
                        // no popup there is nothing to unwind.
                        if self.popup.is_open() {
                            self.close_popup();
                        }
                    }
                    KeyAction::Submit => self.pending_shell_submit = true,
                    _ => {}
                }
            }
            return;
        }
        match action {
            KeyAction::Text(text) => {
                self.search_text.push_str(&text);
                self.search.query(&self.search_text);
                // Fresh results obsolete the failed row, the cached
                // hits, and the result cursor.
                self.overview_failure = None;
                self.overview_hits.clear();
                self.reset_overview_cursor();
            }
            KeyAction::Erase => {
                self.search_text.pop();
                self.search.query(&self.search_text);
                // Fresh results obsolete the failed row, the cached
                // hits, and the result cursor.
                self.overview_failure = None;
                self.overview_hits.clear();
                self.reset_overview_cursor();
            }
            KeyAction::Submit => self.pending_submit = true,
            KeyAction::Dismiss => {
                // Split Escape: an open popup closes first and hands
                // focus back to the parked cursor; only with no popup
                // does Escape dismiss the overview (returning to the
                // previously focused window), never both at once.
                if self.popup.is_open() {
                    self.close_popup();
                } else {
                    self.pending_dismiss = true;
                }
            }
            KeyAction::None => {}
            // Navigation queued above; unreachable here.
            KeyAction::Up
            | KeyAction::Down
            | KeyAction::Left
            | KeyAction::Right
            | KeyAction::Next
            | KeyAction::Previous
            | KeyAction::CycleRegion
            | KeyAction::FocusDock
            | KeyAction::FocusPanel => {}
        }
    }

    /// Nonblocking drain of queued navigation actions (task 1
    /// records, the focus model in task 2 consumes).
    pub fn take_nav(&mut self) -> Vec<KeyAction> {
        std::mem::take(&mut self.pending_nav)
    }

    /// Consume a pending closed-overview Enter activation, if any.
    pub fn take_shell_submit(&mut self) -> bool {
        std::mem::take(&mut self.pending_shell_submit)
    }

    /// Keyboard focus cursor, if any.
    pub fn shell_focus(&self) -> Option<ShellFocus> {
        self.focus
    }

    /// Panel strip stops: clock, three tiles, then one stop per
    /// hosted indicator cell. Dynamic like the dock row: unhosted
    /// cells settle away.
    fn panel_stop_count(&self) -> usize {
        PANEL_STOPS + self.indicators.items().len()
    }

    /// Stop count for `region`: panel stops track clock, tiles, and
    /// hosted indicators; dock stops track the live item row; popup
    /// stops track the open menu's rows; overview stops track the
    /// collected hits.
    fn stop_count(&self, region: FocusRegion) -> usize {
        match region {
            FocusRegion::Panel => self.panel_stop_count(),
            FocusRegion::Dock => self.dock_items().len(),
            FocusRegion::Popup => self.popup_row_count(),
            FocusRegion::Overview => self.overview_hits.len(),
        }
    }

    /// True while `region` can hold the cursor: the strip always
    /// can; the dock needs an item; popups and the overview need
    /// to be open.
    fn region_live(&self, region: FocusRegion) -> bool {
        match region {
            FocusRegion::Panel => true,
            FocusRegion::Dock => !self.dock_items().is_empty(),
            FocusRegion::Popup => self.popup.is_open(),
            FocusRegion::Overview => self.model.is_overview_open(),
        }
    }

    /// Actionable rows in the open popup: two calendar footers,
    /// two network toggles, one lock row, three sound rows, or the
    /// hosted indicator menu's row count. Zero with no popup.
    fn popup_row_count(&self) -> usize {
        match self.popup.body() {
            None => 0,
            Some(PopupBody::Calendar) => 2,
            Some(PopupBody::Menu(index)) if index == NETWORK_TILE_INDEX => 2,
            Some(PopupBody::Menu(index)) if index == POWER_TILE_INDEX => 1,
            Some(PopupBody::Menu(index)) if index == SOUND_TILE_INDEX => 3,
            Some(PopupBody::Menu(_)) => 0,
            Some(PopupBody::IndicatorMenu(index)) => self
                .indicators
                .items()
                .get(index)
                .map(|item| item.menu.len())
                .unwrap_or(0),
        }
    }

    /// Park the live cursor for Escape: opening a popup or the
    /// overview pushes here so dismissal restores exactly this
    /// stop. Bounded and duplicate-free at the top.
    fn push_focus(&mut self) {
        if let Some(focus) = self.focus {
            if self.return_stack.last() != Some(&focus) {
                self.return_stack.push(focus);
            }
            while self.return_stack.len() > 8 {
                self.return_stack.remove(0);
            }
        }
    }

    /// Settle the cursor after any state change: a cursor on a dead
    /// surface (closed popup or overview, emptied dock) falls back
    /// to the return stack, and dead stack tops drain with it, so
    /// focus never sticks where no stop exists. A live cursor is
    /// clamped into its stops and left alone.
    fn settle_focus(&mut self) {
        loop {
            let Some(focus) = self.focus else {
                return;
            };
            if !self.region_live(focus.region) {
                self.focus = self.return_stack.pop();
                continue;
            }
            let max = match focus.region {
                FocusRegion::Panel => self.panel_stop_count(),
                FocusRegion::Dock => self.dock_items().len(),
                FocusRegion::Popup => self.popup_row_count(),
                // Result rows stream in: the cursor clamps at move,
                // paint, and activation time, never here.
                FocusRegion::Overview => usize::MAX,
            };
            if focus.cursor >= max {
                self.focus = Some(ShellFocus {
                    cursor: max.saturating_sub(1),
                    ..focus
                });
            }
            while let Some(&top) = self.return_stack.last() {
                if self.region_live(top.region) {
                    break;
                }
                self.return_stack.pop();
            }
            return;
        }
    }

    /// Drain newly arrived hub answers behind the overview cursor,
    /// capped at the hub's own bound (oldest first, like submit
    /// would have seen them).
    fn refresh_overview_hits(&mut self) {
        let hits = self.search_collect();
        self.overview_hits.extend(hits);
        let len = self.overview_hits.len();
        if len > MAX_TOTAL_RESULTS {
            self.overview_hits.drain(..len - MAX_TOTAL_RESULTS);
        }
    }

    /// Move the overview result cursor one row along `direction`
    /// (±1), wrapping. From any other region the cursor starts at
    /// the first (or, backward, the last) collected hit.
    fn step_overview(&mut self, direction: i32) {
        self.refresh_overview_hits();
        let count = self.overview_hits.len();
        let cursor = match self.focus {
            Some(focus) if focus.region == FocusRegion::Overview => {
                if count == 0 {
                    0
                } else {
                    (focus.cursor as i32 + direction).rem_euclid(count as i32) as usize
                }
            }
            _ => {
                if direction < 0 {
                    count.saturating_sub(1)
                } else {
                    0
                }
            }
        };
        self.focus = Some(ShellFocus {
            region: FocusRegion::Overview,
            cursor,
        });
    }

    /// Restart the result cursor on a re-query: fresh answers
    /// obsolete the old row. Other regions keep their cursor.
    fn reset_overview_cursor(&mut self) {
        if let Some(focus) = self.focus {
            if focus.region == FocusRegion::Overview {
                self.focus = Some(ShellFocus { cursor: 0, ..focus });
            }
        }
    }

    /// Result the next Enter activates: the focused overview row,
    /// or the top hit when focus sits anywhere else. Refreshes
    /// first, so keyboard and pointer activate the same rows.
    fn take_overview_hit(&mut self) -> Option<SearchResult> {
        self.refresh_overview_hits();
        let index = match self.focus {
            Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor,
            }) => cursor,
            _ => 0,
        };
        self.overview_hits.get(index).cloned()
    }

    /// Enter `region`, keeping the cursor when already there
    /// (Alt+F1 re-presses don't lose the tile).
    fn focus_region(&mut self, region: FocusRegion) {
        let cursor = match self.focus {
            Some(focus) if focus.region == region => focus.cursor,
            _ => 0,
        };
        self.focus = Some(ShellFocus { region, cursor });
        self.settle_focus();
    }

    /// Move the cursor one stop along the strip, wrapping. From no
    /// focus, forward lands on the first panel stop and backward on
    /// the last (indicator cells included while hosted).
    fn step_cursor(&mut self, direction: i32) {
        let Some(focus) = self.focus else {
            let cursor = if direction < 0 {
                self.panel_stop_count().saturating_sub(1)
            } else {
                0
            };
            self.focus = Some(ShellFocus {
                region: FocusRegion::Panel,
                cursor,
            });
            return;
        };
        let count = self.stop_count(focus.region);
        if count == 0 {
            self.focus = None;
            return;
        }
        let cursor = (focus.cursor as i32 + direction).rem_euclid(count as i32) as usize;
        self.focus = Some(ShellFocus { cursor, ..focus });
    }

    /// Apply one queued navigation action to the focus cursor. Up
    /// and Down cross between strip and dock; Left/Right and
    /// Tab/Shift+Tab walk the stops; F6 flips the region. Inside
    /// an open popup every movement key walks its rows instead —
    /// the focused surface owns the arrows. While the overview is
    /// open the result list owns movement (arrows and Tab walk
    /// rows); the region keys still jump the shell.
    fn apply_nav(&mut self, action: KeyAction) {
        // The focused surface owns movement: inside an open
        // popup every movement key walks its rows, consuming the
        // press; region keys fall through to the jump table below.
        if matches!(
            self.focus,
            Some(ShellFocus {
                region: FocusRegion::Popup,
                ..
            })
        ) {
            match action {
                KeyAction::Next | KeyAction::Right | KeyAction::Down => {
                    self.step_cursor(1);
                    return;
                }
                KeyAction::Previous | KeyAction::Left | KeyAction::Up => {
                    self.step_cursor(-1);
                    return;
                }
                _ => {}
            }
        }
        if self.model.is_overview_open() {
            match action {
                KeyAction::Next
                | KeyAction::Previous
                | KeyAction::Up
                | KeyAction::Down
                | KeyAction::Left
                | KeyAction::Right => {
                    let forward =
                        matches!(action, KeyAction::Next | KeyAction::Down | KeyAction::Right);
                    self.step_overview(if forward { 1 } else { -1 });
                    return;
                }
                _ => {}
            }
        }
        match action {
            KeyAction::FocusPanel => self.focus_region(FocusRegion::Panel),
            KeyAction::FocusDock => self.focus_region(FocusRegion::Dock),
            KeyAction::CycleRegion => {
                let region = match self.focus {
                    Some(focus) if focus.region == FocusRegion::Panel => FocusRegion::Dock,
                    _ => FocusRegion::Panel,
                };
                // Never land on an empty region (an itemless dock):
                // stay where the cursor is live.
                if self.stop_count(region) > 0 {
                    self.focus = Some(ShellFocus { region, cursor: 0 });
                }
                self.settle_focus();
            }
            KeyAction::Up => self.focus_region(FocusRegion::Panel),
            KeyAction::Down => self.focus_region(FocusRegion::Dock),
            KeyAction::Next | KeyAction::Right => self.step_cursor(1),
            KeyAction::Previous | KeyAction::Left => self.step_cursor(-1),
            _ => {}
        }
    }

    /// Drain the queued navigation actions into the focus cursor.
    /// Shell-local: no round-trip, no wire. Paint keys carry the
    /// cursor, so the ring follows on the next tick.
    fn step_focus(&mut self) {
        for action in self.take_nav() {
            self.apply_nav(action);
        }
        self.settle_focus();
    }

    /// Activate the focused stop through the pointer-equivalent
    /// call: a panel stop toggles its popup like a strip press, a
    /// dock slot takes a left press (launch, switch, or stack).
    /// No-op without focus.
    pub fn activate_focused(&mut self) {
        self.settle_focus();
        let Some(focus) = self.focus else {
            return;
        };
        match focus.region {
            FocusRegion::Panel => {
                if focus.cursor >= PANEL_STOPS {
                    // Hosted indicator cell: the shared strip-press
                    // call (empty menus activate in place, menus
                    // open), then row focus like the tile popups.
                    self.activate_indicator_stop(focus.cursor - PANEL_STOPS);
                } else {
                    let body = if focus.cursor == 0 {
                        PopupBody::Calendar
                    } else {
                        PopupBody::Menu(focus.cursor - 1)
                    };
                    self.popup.toggle(body);
                    self.apply_popup_size();
                }
                if self.popup.is_open() {
                    // A keyboard-opened popup takes row focus,
                    // parking the strip cursor for Escape.
                    self.push_focus();
                    self.focus = Some(ShellFocus {
                        region: FocusRegion::Popup,
                        cursor: 0,
                    });
                } else {
                    self.settle_focus();
                }
            }
            FocusRegion::Dock => {
                let items = self.dock_items();
                if focus.cursor < items.len() {
                    self.run_dock_press(&items, focus.cursor, BTN_LEFT);
                }
            }
            FocusRegion::Popup => {
                self.activate_popup_row(focus.cursor);
            }
            // Overview Enter owns Submit while open; a closed
            // overview can never hold this region (settle pops
            // it). Defensive no-op.
            FocusRegion::Overview => {}
        }
    }

    /// Activate hosted indicator cell `index` like a strip press
    /// on its cell: dismiss when its menu is already open, else
    /// open through the shared
    /// [`open_indicator_menu`](Self::open_indicator_menu) (empty
    /// menus activate in place, fetched menus open). Out-of-range
    /// cells are quiet no-ops.
    fn activate_indicator_stop(&mut self, index: usize) {
        if self.indicators.items().get(index).is_none() {
            return;
        }
        if self.popup.body() == Some(PopupBody::IndicatorMenu(index)) {
            self.close_popup();
        } else {
            self.open_indicator_menu(index);
            self.apply_popup_size();
        }
    }

    /// Activate open-popup row `cursor` through the
    /// pointer-equivalent fire calls: calendar footers flip, menu
    /// rows toggle, indicator rows send and dismiss (pointer
    /// parity) so Escape unwinds to the parked shell focus.
    /// Out-of-range rows are quiet no-ops.
    fn activate_popup_row(&mut self, cursor: usize) {
        match self.popup.body() {
            Some(PopupBody::Calendar) => self.fire_calendar_index(cursor),
            Some(PopupBody::Menu(index)) if index == NETWORK_TILE_INDEX => {
                self.fire_network_index(cursor);
            }
            Some(PopupBody::Menu(index)) if index == POWER_TILE_INDEX => {
                self.fire_lock_index(cursor);
            }
            Some(PopupBody::Menu(index)) if index == SOUND_TILE_INDEX => {
                self.fire_sound_index(cursor);
            }
            Some(PopupBody::IndicatorMenu(index)) => {
                self.fire_indicator_index(index, cursor);
                self.close_popup();
            }
            _ => {}
        }
        self.apply_popup_size();
    }

    /// Drop in-flight answers (e.g. overview closed mid-query).
    pub fn search_cancel(&self) {
        self.search.cancel();
    }

    /// Nonblocking drain of the current answer, merged and bounded.
    pub fn search_collect(&mut self) -> Vec<SearchResult> {
        self.search.collect()
    }

    /// Inline launch failure awaiting repaint, if the last overview
    /// press failed to spawn.
    pub fn overview_failure(&self) -> Option<&OverviewFailure> {
        self.overview_failure.as_ref()
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

    /// Lock the session at once from the shell trigger surface
    /// (manual lock).
    ///
    /// Sends the tokenless `Lock` command over the control channel
    /// like [`activate_hit`](Self::activate_hit) sends its commands:
    /// a send failure reports [`HitError::Control`] and nothing is
    /// flipped locally — the lock screen engages when the
    /// compositor's locked snapshot lands (read through
    /// [`ControlClient::locked`](crate::control::ControlClient::locked)).
    /// Returns the request id for `CommandResult` correlation.
    pub fn lock_now(&self, control: &mut ControlClient) -> Result<u64, HitError> {
        control.lock().map_err(HitError::Control)
    }

    /// Press overview result `index` (the app-launch and
    /// action-run press path, plus inline failure reporting).
    ///
    /// Routes through [`overview_press`] and runs the routed hit
    /// through [`activate_hit`](Self::activate_hit): app results
    /// resolve by desktop-entry id and spawn through the shared
    /// [`LaunchTracker`](crate::apps::LaunchTracker), dismissing the
    /// overview on success while starting feedback flows from
    /// [`launch_states`](Self::launch_states) exactly as dock
    /// launches do; window (action) results run their activation
    /// command through the token gate and dismiss on success. A
    /// press that routes nowhere (wrong button, stale index) is a
    /// `None` no-op. A spawn failure reports [`HitError::Launch`]
    /// with the overview still open and records the pressed row in
    /// [`overview_failure`](Self::overview_failure) so the next
    /// repaint marks it inline — fail-closed like
    /// [`LaunchTracker`](crate::apps::LaunchTracker): nothing
    /// tracked, nothing dismissed. This path never panics.
    pub fn press_overview(
        &mut self,
        control: &mut ControlClient,
        results: &[SearchResult],
        index: usize,
        button: u32,
    ) -> Result<Option<HitOutcome>, HitError> {
        if overview_press(results, index, button).is_none() {
            return Ok(None);
        }
        let Some(hit) = results.get(index) else {
            return Ok(None);
        };
        match self.activate_hit(control, hit) {
            Ok(outcome) => {
                // Success dismisses: no stale marker survives, and
                // the key drops so a still-open overview (best-effort
                // dismissal) repaints any previous marker away.
                self.overview_failure = None;
                self.paint_key = None;
                Ok(Some(outcome))
            }
            Err(HitError::Launch(err)) => {
                let message = format!("launch failed: {err}");
                self.overview_failure = Some(OverviewFailure { index, message });
                // The marker is new paint: force the next frame.
                self.paint_key = None;
                Err(HitError::Launch(err))
            }
            Err(other) => Err(other),
        }
    }

    /// Build one panel `wl_surface` plus its top-anchored layer
    /// surface, bound to `output` (`None` leaves placement to the
    /// compositor, which falls back to primary), from already-bound
    /// handles. Returns the pair for the caller to store (primary
    /// fields or an extra record).
    fn build_panel_surface(
        &self,
        output: Option<&WlOutput>,
    ) -> Option<(WlSurface, ZwlrLayerSurfaceV1)> {
        let wayland = self.wayland.clone()?;
        Some(make_panel_surface(
            &wayland.compositor,
            &wayland.layer_shell,
            &wayland.qh,
            &self.panel.namespace,
            self.panel.height,
            output,
        ))
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
        // Startup path keeps the legacy unbound surface; the tick
        // rebinds it once the inventory and `wl_output` names land.
        let (surface, layer_surface) = make_panel_surface(
            compositor,
            layer_shell,
            qh,
            &self.panel.namespace,
            self.panel.height,
            None,
        );
        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);
    }

    /// Build one dock `wl_surface` plus its bottom-anchored layer
    /// surface, bound to `output`, from already-bound handles.
    /// Overlay layer with no exclusive zone: the dock floats over
    /// windows like the switcher, so maximized geometry is untouched.
    fn build_dock_surface(
        &self,
        output: Option<&WlOutput>,
    ) -> Option<(WlSurface, ZwlrLayerSurfaceV1)> {
        let wayland = self.wayland.clone()?;
        Some(make_dock_surface(
            &wayland.compositor,
            &wayland.layer_shell,
            &wayland.qh,
            output,
        ))
    }

    /// Create the dock `wl_surface` plus its bottom-anchored layer
    /// surface. Overlay layer with no exclusive zone: the dock floats
    /// over windows like the switcher, so maximized geometry is
    /// untouched.
    fn create_dock_surface(
        &mut self,
        compositor: &WlCompositor,
        layer_shell: &ZwlrLayerShellV1,
        qh: &QueueHandle<Self>,
    ) {
        // Startup path keeps the legacy unbound surface; the tick
        // rebinds it once the inventory and `wl_output` names land.
        let (surface, layer_surface) = make_dock_surface(compositor, layer_shell, qh, None);
        self.dock_surface = Some(surface);
        self.dock_layer = Some(layer_surface);
    }

    /// Reconcile live surfaces with the compositor inventory: one
    /// panel per bound output (primary keeps the legacy fields, the
    /// rest become extras), the dock on the primary output. Unknown
    /// inventory (empty list) keeps the legacy unbound surfaces.
    /// Idempotent: the tick calls this after every control step, and
    /// only missing or surplus surfaces change.
    fn reconcile_outputs(&mut self, outputs: &[OutputInfo]) {
        if outputs.is_empty() || self.wayland.is_none() {
            return;
        }
        let bound: Vec<String> = self
            .bound_outputs
            .iter()
            .filter_map(|bound| bound.name.clone())
            .collect();
        let plan = plan_output_surfaces(outputs, &bound);
        let primary = outputs
            .iter()
            .find(|info| info.primary)
            .map(|info| info.name.clone());
        // Primary panel: rebind when it names a different bound
        // output than the live surface holds.
        if let Some(name) = primary.clone() {
            if self.panel_output.as_ref() != Some(&name) {
                if let Some(target) = self
                    .bound_outputs
                    .iter()
                    .find(|b| b.name.as_ref() == Some(&name))
                {
                    let output = target.output.clone();
                    self.surface = None;
                    self.layer_surface = None;
                    self.panel_size = None;
                    self.panel_backing = None;
                    self.panel_paint_key = None;
                    if let Some((surface, layer)) = self.build_panel_surface(Some(&output)) {
                        self.surface = Some(surface);
                        self.layer_surface = Some(layer);
                        self.panel_output = Some(name);
                    }
                }
            }
        }
        // Extra panels: create per non-primary plan entry, drop the
        // rest (removed outputs or lost bindings).
        let wanted: Vec<String> = plan
            .panels
            .iter()
            .filter(|name| Some(*name) != primary.as_ref())
            .cloned()
            .collect();
        self.extra_panels
            .retain(|extra| wanted.iter().any(|name| name == &extra.name));
        for name in wanted {
            if self.extra_panels.iter().any(|extra| extra.name == name) {
                continue;
            }
            let Some(target) = self
                .bound_outputs
                .iter()
                .find(|b| b.name.as_ref() == Some(&name))
            else {
                continue;
            };
            let output = target.output.clone();
            if let Some((surface, layer)) = self.build_panel_surface(Some(&output)) {
                self.extra_panels.push(ExtraPanel {
                    name,
                    surface,
                    layer,
                    size: None,
                    backing: None,
                    paint_key: None,
                });
            }
        }
        // Dock: follow the primary anchor, rebinding on change.
        // While the primary is unbound, keep the legacy surface until
        // its `wl_output` resolves.
        if self.dock_output.as_ref() != plan.dock.as_ref() {
            if let Some(name) = plan.dock.clone() {
                if let Some(target) = self
                    .bound_outputs
                    .iter()
                    .find(|b| b.name.as_ref() == Some(&name))
                {
                    let output = target.output.clone();
                    self.dock_surface = None;
                    self.dock_layer = None;
                    self.dock_size = None;
                    self.dock_backing = None;
                    self.dock_paint_key = None;
                    if let Some((surface, layer)) = self.build_dock_surface(Some(&output)) {
                        self.dock_surface = Some(surface);
                        self.dock_layer = Some(layer);
                        self.dock_output = Some(name);
                    }
                }
            }
        }
    }

    /// Bind one advertised `wl_output` for per-output surfaces, plus
    /// its `xdg_output` tracker when the manager is already known.
    /// Names resolve asynchronously through the tracker events.
    fn bind_output(
        &mut self,
        registry: &wl_registry::WlRegistry,
        global: u32,
        version: u32,
        qh: &QueueHandle<Self>,
    ) {
        if self
            .bound_outputs
            .iter()
            .any(|bound| bound.global == global)
        {
            return;
        }
        let output: WlOutput = registry.bind(global, version.min(4), qh, ());
        let xdg = self
            .xdg_manager
            .as_ref()
            .map(|manager| manager.get_xdg_output(&output, qh, global));
        self.bound_outputs.push(BoundOutput {
            global,
            output,
            _xdg: xdg,
            name: None,
        });
    }

    /// Bind the `xdg_output` manager and attach trackers to
    /// already-bound outputs still awaiting names.
    fn bind_xdg_manager(
        &mut self,
        registry: &wl_registry::WlRegistry,
        global: u32,
        version: u32,
        qh: &QueueHandle<Self>,
    ) {
        if self.xdg_manager.is_some() {
            return;
        }
        let manager: ZxdgOutputManagerV1 = registry.bind(global, version.min(3), qh, ());
        for bound in &mut self.bound_outputs {
            if bound._xdg.is_none() {
                bound._xdg = Some(manager.get_xdg_output(&bound.output, qh, bound.global));
            }
        }
        self.xdg_manager = Some(manager);
    }

    /// Drop a removed output global; the tick's reconcile destroys
    /// its surfaces (dropping the proxies sends the protocol
    /// destroy).
    fn unbind_output(&mut self, global: u32) {
        self.bound_outputs.retain(|bound| bound.global != global);
    }

    /// Current dock items from favorites, desktop entries, and the
    /// compositor window list.
    fn dock_items(&self) -> Vec<DockItem> {
        dock_items(self.favorites.ids(), &self.apps, self.model.windows())
    }

    /// Publish the snapshot wallpaper URI to the compositor drop
    /// file when it changes. Best-effort like favorites saves: a
    /// failed write leaves the last published file in place and the
    /// compositor keeps its current image.
    fn publish_wallpaper(&mut self) {
        let current = self.tiles.settings.wallpaper_uri.clone();
        if current == self.published_wallpaper {
            return;
        }
        let path = self.tiles.runtime_dir().join("roost-wallpaper");
        let done = match &current {
            Some(uri) => std::fs::write(&path, uri).is_ok(),
            None => std::fs::remove_file(&path).is_ok(),
        };
        if done {
            self.published_wallpaper = current;
        }
    }

    /// Sync the artwork cache with the snapshot theme: a theme flip
    /// clears the cache and bumps both paint revisions so bars and
    /// docks repaint with the new artwork on this same tick.
    fn sync_icon_theme(&mut self) {
        let current = self.tiles.settings.icon_theme.clone();
        if current == self.icon_theme {
            return;
        }
        self.icon_theme = current;
        self.icon_cache.clear();
        self.panel_paint_key = None;
        self.dock_paint_key = None;
    }

    /// Resolve `name` to artwork at `px` through the host cache,
    /// falling back to a fresh lookup on a miss. `None` means the
    /// caller keeps its placeholder.
    pub fn icon_art(&mut self, name: &str, px: u32) -> Option<Artwork> {
        let key = (self.icon_theme.clone(), name.to_owned(), px);
        if let Some(art) = self.icon_cache.get(&key) {
            return Some(art.clone());
        }
        let art = crate::icons::resolve(&self.icon_theme, name, px)?;
        self.icon_cache.insert(key, art.clone());
        // Bound the cache: theme art is small, but a hostile name
        // stream must not grow it without limit.
        if self.icon_cache.len() > 512 {
            self.icon_cache.clear();
        }
        Some(art)
    }

    /// Select an item's icon from its properties: attention pixmap,
    /// normal pixmap, attention name, normal name, then the service
    /// fallback. Names resolve through the artwork cache; symbolic
    /// names re-tint toward the shell accent at fetch time.
    fn build_icon(&mut self, info: &ItemInfo) -> IndicatorIcon {
        if info.needs_attention() {
            if let Some(icon) = info.attention_pixmap_icon() {
                return icon;
            }
        }
        if let Some(icon) = info.pixmap_icon() {
            return icon;
        }
        if info.needs_attention() {
            if let Some(icon) = self.cached_name(&info.attention_name) {
                return icon;
            }
        }
        if let Some(icon) = self.cached_name(&info.icon_name) {
            return icon;
        }
        IndicatorIcon::Named(info.service.clone())
    }

    /// Resolve a theme name through the cache with symbolic
    /// re-tint. Empty names miss so the caller falls through.
    fn cached_name(&mut self, name: &str) -> Option<IndicatorIcon> {
        if name.is_empty() {
            return None;
        }
        let art = self.icon_art(name, crate::watcher::INDICATOR_CELL as u32)?;
        let art = if name.ends_with("-symbolic") {
            crate::icons::retint(&art, crate::overview::ACCENT)
        } else {
            art
        };
        Some(IndicatorIcon::Pixmap {
            width: art.size as i32,
            height: art.size as i32,
            argb: art.argb,
        })
    }

    /// Resolve a dock item's app artwork through the host cache:
    /// the desktop entry's icon name at dock size, with symbolic
    /// names re-tinted like tray icons. `None` keeps the
    /// initial-letter fallback (unknown entries, stacks, misses).
    fn dock_icon(&mut self, item: &DockItem) -> Option<Artwork> {
        let name = self.apps.entry(&item.app_id)?.icon.clone()?;
        if name.is_empty() {
            return None;
        }
        let art = self.icon_art(&name, crate::dock::DOCK_ICON_PX)?;
        Some(if name.ends_with("-symbolic") {
            crate::icons::retint(&art, ACCENT)
        } else {
            art
        })
    }

    /// Poll the indicator edge: take the watcher role when free,
    /// drop vanished clients, and refresh hosted icons. Empty host
    /// without a bus, by design.
    fn poll_indicators(&mut self) {
        if !self.watcher.ensure() {
            return;
        }
        let before = self.indicators.items().to_vec();
        let registered = self.watcher.registered();
        let live = self.watcher.prune_vanished(&registered);
        let gone: Vec<String> = registered
            .iter()
            .filter(|service| !live.contains(service))
            .cloned()
            .collect();
        self.watcher.forget(&gone);
        for service in &live {
            if let Some(info) = self.watcher.fetch_info(service) {
                let icon = self.build_icon(&info);
                let menu = self
                    .indicators
                    .get(service)
                    .map(|item| item.menu.clone())
                    .unwrap_or_default();
                self.indicators.upsert(crate::watcher::IndicatorItem {
                    service: info.service.clone(),
                    title: info.title.clone(),
                    icon,
                    menu,
                });
            }
        }
        self.indicators.retain_registered(&live);
        // An open menu follows its item's rows.
        if let Some(PopupBody::IndicatorMenu(index)) = self.popup.body() {
            if let Some(service) = self
                .indicators
                .items()
                .get(index)
                .map(|item| item.service.clone())
            {
                let menu = self.watcher.fetch_menu(&service);
                if let Some(mut item) = self.indicators.get(&service).cloned() {
                    item.menu = menu;
                    self.indicators.upsert(item);
                }
            }
        }
        if self.indicators.items() != before.as_slice() {
            self.panel_paint_key = None;
        }
    }

    /// Take the notifications role when free: the intake edge files
    /// wire arrivals into the shared center on its own threads, so
    /// the tick only ensures the name. Silent no-op without a bus.
    fn poll_notifications(&mut self) {
        self.notifications.ensure();
    }

    /// Open an indicator's menu, fetching its rows first. An empty
    /// menu activates the item directly instead of opening nothing.
    fn open_indicator_menu(&mut self, index: usize) {
        let Some(service) = self
            .indicators
            .items()
            .get(index)
            .map(|item| item.service.clone())
        else {
            return;
        };
        let menu = self.watcher.fetch_menu(&service);
        if menu.is_empty() {
            self.watcher.activate(&service);
            self.popup.dismiss();
        } else {
            if let Some(mut item) = self.indicators.get(&service).cloned() {
                item.menu = menu;
                self.indicators.upsert(item);
            }
            self.popup.open(PopupBody::IndicatorMenu(index));
        }
        self.panel_paint_key = None;
    }

    /// Fire the menu row under a press inside an open indicator menu,
    /// then dismiss either way.
    fn fire_indicator_row(&mut self, index: usize, y: i32, open_box: &crate::popup::Rect) {
        if let Some(item) = self.indicators.items().get(index).cloned() {
            if let Some(row) = menu_row_at(open_box, y, item.menu.len()) {
                self.fire_indicator_index(index, row);
            }
        }
        self.popup.dismiss();
        self.panel_paint_key = None;
    }

    /// Fire indicator-menu row `row` of hosted item `index` by row
    /// number: the keyboard twin of the menu press path, sharing its
    /// enabled gating and bus send. The caller dismisses: pointer
    /// presses dismiss in `press_panel`, keyboard activation
    /// dismisses so Escape can unwind to the parked shell focus.
    fn fire_indicator_index(&mut self, index: usize, row: usize) {
        if let Some(item) = self.indicators.items().get(index).cloned() {
            if let Some(entry) = item.menu.get(row) {
                if entry.enabled {
                    self.watcher.fire_menu(&item.service, entry.id);
                }
            }
        }
    }

    /// Surface height for the open stack grid: strip-only, or strip
    /// plus the grid band while a stack is open.
    fn dock_surface_height(&self) -> i32 {
        DOCK_H
            + if self.open_stack.is_some() {
                STACK_GRID_H
            } else {
                0
            }
    }

    /// Directory backing the open stack item, if it still resolves.
    fn open_stack_dir(&self, items: &[DockItem]) -> Option<std::path::PathBuf> {
        let index = self.open_stack?;
        items.get(index)?.stack_dir.clone()
    }

    /// Apply the stack state to the live surface: request the
    /// matching size and repaint at it. The compositor's configure
    /// round trip repaints again at the arranged size; without
    /// Wayland attached (tests) this only flips state.
    fn apply_dock_size(&mut self) {
        let height = self.dock_surface_height();
        if let (Some(layer), Some(surface)) = (self.dock_layer.as_ref(), self.dock_surface.as_ref())
        {
            layer.set_size(0, height as u32);
            surface.commit();
        }
        if let Some((width, _)) = self.dock_size {
            self.dock_paint_key = None;
            self.paint_dock_surface(width, height);
        }
    }

    /// Open (or re-target) the stack grid for item `index`, or close
    /// it with `None`. Refreshes the directory read on open.
    fn set_stack(&mut self, open: Option<usize>) {
        self.open_stack = open;
        self.stack_cache = open
            .and_then(|index| self.dock_items().get(index)?.stack_dir.clone())
            .map(|dir| read_stack(&dir))
            .unwrap_or_default();
        self.apply_dock_size();
    }

    /// Paint key for one dock size: items, pins, size, the open
    /// stack grid, and the keyboard focus cursor, so any content
    /// or focus change repaints.
    fn dock_render_key(&self, width: i32, height: i32, items: &[DockItem]) -> DockPaintKey {
        let stack = self.open_stack.map(|index| {
            (
                index,
                self.stack_cache
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect(),
            )
        });
        DockPaintKey {
            items: items
                .iter()
                .map(|item| {
                    (
                        item.app_id.clone(),
                        item.windows.clone(),
                        item.active,
                        item.pinned,
                    )
                })
                .collect(),
            badges: self.extension_badges(),
            width,
            height,
            stack,
            focus: self.focus,
        }
    }

    /// Paint the dock strip into a fresh shm buffer and attach it.
    /// Repaints when items, focus, pins, size, or the stack grid
    /// change; called on configure and from the slow status tick.
    fn paint_dock_surface(&mut self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        let items = self.dock_items();
        // Refresh the grid read while open: directory contents may
        // change under the shell without any dock event.
        if let Some(dir) = self.open_stack_dir(&items) {
            let fresh = read_stack(&dir);
            if fresh != self.stack_cache {
                self.stack_cache = fresh;
            }
        }
        let key = self.dock_render_key(width, height, &items);
        if self.dock_size == Some((width, height))
            && self.dock_backing.is_some()
            && self.dock_paint_key.as_ref() == Some(&key)
        {
            return;
        }
        let Some(wayland) = self.wayland.clone() else {
            return;
        };
        let pixels = self.render_dock_pixels(width, height, &items);
        let Some(dock) = self.dock_surface.as_ref() else {
            return;
        };
        if let Some(backing) = shm_upload(&wayland.shm, &wayland.qh, &pixels, width, height) {
            dock.attach(Some(&backing.buffer), 0, 0);
            dock.damage(0, 0, width, height);
            dock.commit();
            self.dock_backing = Some(backing);
            self.dock_size = Some((width, height));
            self.dock_paint_key = Some(key);
        }
    }

    /// Pixel content for the dock strip: icon squares, the open
    /// stack grid band, and the keyboard focus ring around the
    /// focused slot. Pure paint shared by the surface tick and the
    /// ring tests (the tick only uploads).
    fn render_dock_pixels(&mut self, width: i32, height: i32, items: &[DockItem]) -> Vec<u8> {
        let mut pixels = vec![0u8; width as usize * height as usize * BYTES_PER_PIXEL];
        let icons: Vec<Option<Artwork>> = items.iter().map(|item| self.dock_icon(item)).collect();
        let grid = if self.open_stack.is_some() {
            Some(self.stack_cache.as_slice())
        } else {
            None
        };
        paint_dock_stacked_with_icons(&mut pixels, width, height, items, grid, &icons);
        // Script badges continue the slot row past the last item (or
        // centered alone when the dock is empty): a dim frame plus the
        // badge text, truncated to the slot. Cached texts only.
        let badges = self.extension_badges();
        if !badges.is_empty() {
            let y_origin = if grid.is_some() { STACK_GRID_H } else { 0 };
            let start_x = if items.is_empty() {
                (width - badges.len() as i32 * DOCK_SLOT) / 2
            } else {
                let (last_x, _, _, _) = dock_slot_at(width, items.len() - 1, items.len(), y_origin);
                last_x + DOCK_SLOT
            };
            paint_script_badges(&mut pixels, width, start_x, y_origin, &badges);
        }
        if let Some(ShellFocus {
            region: FocusRegion::Dock,
            cursor,
        }) = self.focus
        {
            if cursor < items.len() {
                let origin = if self.open_stack.is_some() {
                    STACK_GRID_H
                } else {
                    0
                };
                let (x0, y0, w, h) = dock_slot_at(width, cursor, items.len(), origin);
                paint_focus_ring(&mut pixels, width, &Rect { x: x0, y: y0, w, h });
            }
        }
        pixels
    }

    /// Press at dock-surface coordinates with a mouse button: run
    /// launch/pin immediately, arm switch/close for the control loop.
    /// An open stack grid takes grid-cell presses first; any other
    /// dock press closes the grid. Ignored before the first
    /// configure.
    pub fn press_dock(&mut self, x: i32, y: i32, button: u32) {
        let Some((width, _)) = self.dock_size else {
            return;
        };
        let items = self.dock_items();
        let count = items.len();
        let origin = if self.open_stack.is_some() {
            STACK_GRID_H
        } else {
            0
        };
        // Grid-band presses activate (left) or dismiss (other
        // buttons) a cell, then close the grid either way.
        if self.open_stack.is_some() && y < STACK_GRID_H {
            if button == crate::dock::BTN_LEFT {
                let shown = stack_shown(width, self.stack_cache.len());
                if let Some(cell) = stack_cell_at(width, x, y, shown) {
                    let entry = self.stack_cache[cell].clone();
                    self.activate_stack_entry(&entry);
                }
            }
            self.set_stack(None);
            return;
        }
        let slots = (0..count)
            .map(|index| dock_slot_at(width, index, count, origin))
            .collect::<Vec<_>>();
        let hit = slots
            .iter()
            .position(|(sx, sy, w, h)| x >= *sx && x < *sx + *w && y >= *sy && y < *sy + *h);
        // The grid's own item toggles it on left press; any other
        // strip press (or a miss) closes it before the action runs.
        if self.open_stack.is_some() {
            if hit == self.open_stack && button == crate::dock::BTN_LEFT {
                self.set_stack(None);
                return;
            }
            self.set_stack(None);
        }
        let Some(index) = hit else {
            return;
        };
        self.run_dock_press(&items, index, button);
    }

    /// Launch a stack cell: desktop files through their parsed entry,
    /// plain files through the default handler (`xdg-open`).
    fn activate_stack_entry(&mut self, entry: &StackEntry) {
        if entry.app {
            if let Some(parsed) = entry_from_file(&entry.path) {
                let _ = self.launcher.launch(&parsed);
            }
        } else {
            let synthetic = AppEntry {
                app_id: format!("xdg-open:{}", entry.name),
                name: entry.name.clone(),
                generic_name: None,
                keywords: Vec::new(),
                argv: vec![
                    std::ffi::OsString::from("xdg-open"),
                    entry.path.as_os_str().to_owned(),
                ],
                icon: None,
            };
            let _ = self.launcher.launch(&synthetic);
        }
        self.dock_paint_key = None;
    }

    /// Resolve one icon-strip press to its [`DockAction`] and run it.
    fn run_dock_press(&mut self, items: &[DockItem], index: usize, button: u32) {
        let focused: Vec<u64> = self
            .model
            .windows()
            .iter()
            .filter(|w| w.active)
            .map(|w| w.id)
            .collect();
        let Some(action) = dock_press(items, &focused, index, button) else {
            return;
        };
        match action {
            DockAction::Launch(app_id) => {
                if let Some(entry) = self.apps.entry(&app_id) {
                    let _ = self.launcher.launch(entry);
                }
                self.dock_paint_key = None;
            }
            DockAction::TogglePin(app_id) => {
                if self.favorites.contains(&app_id) {
                    self.favorites.unpin(&app_id);
                } else {
                    self.favorites.pin(&app_id);
                }
                let _ = self.favorites.save();
                // An unpinned stack (or a reorder under the open
                // index) must not leave the grid pointing sideways.
                if self.open_stack_dir(&self.dock_items()).is_none() {
                    self.set_stack(None);
                }
                self.dock_paint_key = None;
            }
            DockAction::OpenStack(index) => {
                self.set_stack(Some(index));
            }
            DockAction::Switch(_) | DockAction::Close(_) => {
                self.pending_dock.push(action);
            }
        }
    }

    /// Take armed dock switch/close actions for the run loop's driver.
    pub fn take_dock_actions(&mut self) -> Vec<DockAction> {
        std::mem::take(&mut self.pending_dock)
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
        let was_open = self.model.is_overview_open();
        self.model.set_overview_open(model.is_overview_open());
        if model.is_overview_open() && !was_open {
            // Fresh open: park the shell cursor and start on the
            // result list, so Escape unwinds back to this stop and
            // the final Escape closes onto the focused window.
            self.push_focus();
            self.focus = Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor: 0,
            });
            self.overview_hits.clear();
        }
        if !model.is_overview_open() {
            // A closed overview shows nothing: never reopen onto a
            // stale failure marker, cached hit, or stranded cursor.
            self.overview_failure = None;
            self.overview_hits.clear();
            self.settle_focus();
        }
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
/// Consume key actions armed by [`ShellHost::on_key`]: queued
/// navigation moves the shell focus cursor first, Enter activates
/// the focused overview row (or the top hit) through the normal
/// path, Escape dismisses via the hub, and closed-overview Enter
/// activates the focused shell stop. Best-effort like the hits
/// themselves: a failed activation must not wedge the loop.
fn drive_key_actions(host: &mut ShellHost, control: &mut ControlClient) {
    // Shell focus moves first: later activations in this tick see
    // the cursor the keys just placed.
    host.step_focus();
    if host.take_submit() {
        if let Some(hit) = host.take_overview_hit() {
            let _ = host.activate_hit(control, &hit);
        }
    }
    if host.take_shell_submit() {
        host.activate_focused();
    }
    if host.take_dismiss() {
        host.close_popup();
        let _ = control.toggle_overview();
    }
}

/// Consume armed dock switch/close actions through the control client.
/// Best-effort like overview hits: failures must not wedge the loop.
/// Launch, pin, and stack toggles already ran locally in
/// [`ShellHost::press_dock`](ShellHost::press_dock); only the actions
/// needing the compositor travel here.
fn drive_dock_actions(host: &mut ShellHost, control: &mut ControlClient) {
    for action in host.take_dock_actions() {
        match action {
            DockAction::Switch(window) => {
                if host.model.select_window(window) {
                    let _ = control.activate_window(window);
                }
            }
            DockAction::Close(window) => {
                let _ = control.close_window(window);
            }
            DockAction::Launch(_) | DockAction::TogglePin(_) | DockAction::OpenStack(_) => {}
        }
    }
}

/// Consume an armed manual lock request through the control client.
/// Best-effort like dock actions: a failed send must not wedge the
/// loop, and nothing flips locally — the lock screen engages when
/// the compositor's locked snapshot lands.
fn drive_lock_actions(host: &mut ShellHost, control: &mut ControlClient) {
    if host.take_lock() {
        let _ = host.lock_now(control);
    }
}

fn drive_control(control: &mut ControlClient, host: &mut ShellHost) {
    match control.poll() {
        Err(e) if is_would_block(&e) => {}
        Ok(Handled::Gap { .. }) => {
            host.sync_overview(control);
            if let Err(e) = control.request_snapshot() {
                eprintln!("roost-shell-host: control resnapshot failed: {e}");
            }
        }
        Ok(_) => host.sync_overview(control),
        Err(e) => eprintln!("roost-shell-host: control error: {e}"),
    }
    // Per-output surfaces follow the inventory every tick, whether or
    // not a control frame landed: registry naming and inventory
    // arrivals race, and reconcile is idempotent.
    let outputs = control.outputs().to_vec();
    host.reconcile_outputs(&outputs);
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ShellHost {
    fn event(
        state: &mut Self,
        proxy: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                // Outputs join live (multi-monitor): bind each
                // `wl_output` for per-output surfaces, plus the
                // `xdg_output` manager that names them. Everything
                // else stays bound once at startup.
                if interface.as_str() == "wl_output" {
                    state.bind_output(proxy, name, version, qh);
                } else if interface.as_str() == "zxdg_output_manager_v1" {
                    state.bind_xdg_manager(proxy, name, version, qh);
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.unbind_output(name);
            }
            _ => {}
        }
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
                    // Configure carries the arranged size: paint the
                    // strip (plus the popup band when one is open) and
                    // commit the buffer with it.
                    state.paint_panel_surface(width as i32, height as i32);
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
                } else if state
                    .banners
                    .as_ref()
                    .is_some_and(|banners| &banners.layer == proxy)
                {
                    proxy.ack_configure(serial);
                    // Size arrives here; the next update pass paints.
                    if let Some(banners) = state.banners.as_mut() {
                        if width > 0 && height > 0 {
                            banners.width = width as i32;
                            banners.height = height as i32;
                        }
                    }
                } else if state.dock_layer.as_ref() == Some(proxy) {
                    proxy.ack_configure(serial);
                    // Size arrives here; the next update pass paints.
                    if width > 0 && height > 0 {
                        state.dock_size = Some((width as i32, height as i32));
                        state.paint_dock_surface(width as i32, height as i32);
                    }
                    if let Some(surface) = state.dock_surface.as_ref() {
                        surface.commit();
                    }
                } else if let Some(index) = state
                    .extra_panels
                    .iter()
                    .position(|extra| &extra.layer == proxy)
                {
                    proxy.ack_configure(serial);
                    // Extra panel configure: paint this output's strip
                    // at its own arranged size, then commit it.
                    if width > 0 && height > 0 {
                        state.paint_extra_panel(index, width as i32, height as i32);
                    }
                    if let Some(extra) = state.extra_panels.get(index) {
                        extra.surface.commit();
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
                } else if state
                    .banners
                    .as_ref()
                    .is_some_and(|banners| &banners.layer == proxy)
                {
                    state.destroy_banners();
                } else if let Some(index) = state
                    .extra_panels
                    .iter()
                    .position(|extra| &extra.layer == proxy)
                {
                    // Losing an extra panel drops its record; the tick
                    // rebuilds it while its output stays inventoried.
                    state.extra_panels.remove(index);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for ShellHost {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: SeatEvent,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // Late input capability (e.g. seat bound before the
        // compositor attached input): acquire once, then hold.
        if let SeatEvent::Capabilities { capabilities } = event {
            let offers_keyboard = matches!(
                capabilities,
                WEnum::Value(caps) if caps.contains(Capability::Keyboard)
            );
            if offers_keyboard && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            let offers_pointer = matches!(
                capabilities,
                WEnum::Value(caps) if caps.contains(Capability::Pointer)
            );
            if offers_pointer && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for ShellHost {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: KeyEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            KeyEvent::Keymap { format, fd, size } => {
                if matches!(format, WEnum::Value(KeymapFormat::XkbV1)) {
                    if let Some(feed) = XkbFeed::from_fd(&fd, size) {
                        state.xkb = Some(feed);
                    }
                }
            }
            KeyEvent::Enter { .. } | KeyEvent::Leave { .. } => {}
            KeyEvent::Key {
                key,
                state: key_state,
                ..
            } => {
                let pressed = matches!(key_state, WEnum::Value(KeyState::Pressed));
                state.on_key(key, pressed);
            }
            KeyEvent::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(feed) = state.xkb.as_mut() {
                    feed.update_mask(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            _ => {}
        }
    }
}

/// Left mouse button (evdev): the only press the panel acts on.
const BTN_LEFT: u32 = 0x110;

impl Dispatch<WlPointer, ()> for ShellHost {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: PointerEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            PointerEvent::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                if Some(&surface) == state.surface.as_ref() {
                    state.pointer_pos = Some((surface_x, surface_y));
                    state.pointer_on_dock = false;
                    state.pointer_on_banner = false;
                } else if Some(&surface) == state.dock_surface.as_ref() {
                    state.pointer_pos = Some((surface_x, surface_y));
                    state.pointer_on_dock = true;
                    state.pointer_on_banner = false;
                } else if state
                    .banners
                    .as_ref()
                    .is_some_and(|banners| banners.surface == surface)
                {
                    state.pointer_pos = Some((surface_x, surface_y));
                    state.pointer_on_dock = false;
                    state.pointer_on_banner = true;
                }
            }
            PointerEvent::Leave { surface, .. } => {
                let tracked = Some(&surface) == state.surface.as_ref()
                    || Some(&surface) == state.dock_surface.as_ref()
                    || state
                        .banners
                        .as_ref()
                        .is_some_and(|banners| banners.surface == surface);
                if tracked {
                    state.pointer_pos = None;
                    state.pointer_on_dock = false;
                    state.pointer_on_banner = false;
                }
            }
            PointerEvent::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                // Motion carries no surface: only meaningful while the
                // pointer is over our surface from a tracked enter.
                if state.pointer_pos.is_some() {
                    state.pointer_pos = Some((surface_x, surface_y));
                }
            }
            PointerEvent::Button {
                button,
                state: pressed,
                ..
            } => {
                let pressed_now = matches!(pressed, WEnum::Value(ButtonState::Pressed));
                if let (true, Some((x, y))) = (pressed_now, state.pointer_pos) {
                    if state.pointer_on_dock {
                        state.press_dock(x as i32, y as i32, button);
                    } else if state.pointer_on_banner {
                        if button == BTN_LEFT {
                            state.press_banner(x as i32, y as i32);
                        }
                    } else if button == BTN_LEFT {
                        state.press_panel(x as i32, y as i32);
                    }
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
delegate_noop!(ShellHost: ignore ZxdgOutputManagerV1);

impl Dispatch<ZxdgOutputV1, u32> for ShellHost {
    fn event(
        state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: XdgOutputEvent,
        global: &u32,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Only the name matters: it keys a bound protocol object to
        // the control inventory. Sizes ride the layer configure, so
        // position/size/done events are ignored.
        if let XdgOutputEvent::Name { name } = event {
            if let Some(bound) = state
                .bound_outputs
                .iter_mut()
                .find(|bound| bound.global == *global)
            {
                bound.name = Some(name);
            }
        }
    }
}
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
/// The compositor sets `ROOST_CONTROL_SOCKET` for the supervised child;
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
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).map_err(PanelError::NoSeat)?;

    let mut host = ShellHost::new(panel, AppProvider::system(), Favorites::system());
    host.load_notification_queue();
    host.load_roost_prefs();
    host.attach_wayland(compositor.clone(), layer_shell.clone(), shm, qh.clone());
    host.attach_seat(seat.clone(), &qh);
    host.create_panel_surface(&compositor, &layer_shell, &qh);
    host.create_dock_surface(&compositor, &layer_shell, &qh);
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
            drive_key_actions(&mut host, control);
            drive_dock_actions(&mut host, control);
            drive_lock_actions(&mut host, control);
            host.update_shell_surfaces(control.revision());
            queue
                .flush()
                .map_err(|e| PanelError::Flush(e.to_string()))?;
            std::thread::sleep(Duration::from_millis(5));
        }
    } else {
        // Panel-only sessions have no control frames; poll Wayland at
        // the same cadence and run the same update pass, so every
        // surface (overview included) paints here too.
        while host.is_running() {
            pump_wayland(&conn, &mut queue, &mut host)?;
            host.update_shell_surfaces(None);
            queue
                .flush()
                .map_err(|e| PanelError::Flush(e.to_string()))?;
            std::thread::sleep(Duration::from_millis(5));
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

    /// Pure per-output surface planner (multi-monitor): panels and the
    /// dock anchor derive from the control inventory plus the bound
    /// `wl_output` names, with no Wayland attached. Hand-written
    /// expectations, never derived from the planner itself.
    mod output_plan {
        use roost_shell_control::OutputInfo;

        use super::super::{plan_output_surfaces, OutputPlan};

        fn info(name: &str, width: i32, height: i32, primary: bool) -> OutputInfo {
            OutputInfo {
                name: name.to_owned(),
                width,
                height,
                primary,
            }
        }

        /// Primary-first inventory as [`State::output_infos`] hands it
        /// over: dock anchor first, sibling second.
        fn inventory() -> Vec<OutputInfo> {
            vec![
                info("left", 1280, 800, true),
                info("right", 1920, 1080, false),
            ]
        }

        #[test]
        fn panels_cover_each_bound_output_primary_first() {
            let plan = plan_output_surfaces(&inventory(), &["left".to_owned(), "right".to_owned()]);
            assert_eq!(
                plan,
                OutputPlan {
                    panels: vec!["left".to_owned(), "right".to_owned()],
                    dock: Some("left".to_owned()),
                }
            );
        }

        #[test]
        fn dock_anchors_on_primary_and_follows_a_switch() {
            let plan = plan_output_surfaces(&inventory(), &["left".to_owned(), "right".to_owned()]);
            assert_eq!(plan.dock.as_deref(), Some("left"));
            // Primary switch re-anchors the dock and keeps panels
            // primary-first.
            let switched = vec![
                info("right", 1920, 1080, true),
                info("left", 1280, 800, false),
            ];
            let plan = plan_output_surfaces(&switched, &["left".to_owned(), "right".to_owned()]);
            assert_eq!(
                plan,
                OutputPlan {
                    panels: vec!["right".to_owned(), "left".to_owned()],
                    dock: Some("right".to_owned()),
                }
            );
        }

        #[test]
        fn unbound_primary_plans_no_dock_but_keeps_bound_panels() {
            // The primary's `wl_output` has not resolved yet: bound
            // siblings still get panels, but the dock waits for the
            // primary rather than parking on a sibling.
            let plan = plan_output_surfaces(&inventory(), &["right".to_owned()]);
            assert_eq!(
                plan,
                OutputPlan {
                    panels: vec!["right".to_owned()],
                    dock: None,
                }
            );
        }

        #[test]
        fn empty_inventory_plans_nothing() {
            // Unknown inventory (e.g. an old compositor that never sends
            // `Outputs`): the legacy unbound surfaces stay live.
            let plan = plan_output_surfaces(&[], &["left".to_owned()]);
            assert_eq!(
                plan,
                OutputPlan {
                    panels: Vec::new(),
                    dock: None,
                }
            );
            let plan = plan_output_surfaces(&inventory(), &[]);
            assert_eq!(
                plan,
                OutputPlan {
                    panels: Vec::new(),
                    dock: None,
                }
            );
        }
    }

    /// Live attach of the real [`ShellHost`] against the compositor's
    /// layer-shell server: the panel surface appears server-side with our
    /// namespace, the configure round-trip acks, and server close stops
    /// the loop. Headless and deterministic: a socketpair client plus a
    /// bounded pump budget, no sleeps.
    mod live {
        use std::os::unix::net::UnixStream;
        use std::rc::Rc;
        use std::sync::Mutex;

        use roost_compositor::{
            control::ControlHub,
            state::{StateModel, TokenStore},
            TestCompositor, SEAT_NAME,
        };
        use wayland_client::{
            protocol::{wl_compositor::WlCompositor, wl_registry, wl_shm::WlShm},
            Connection, Dispatch, EventQueue, QueueHandle,
        };
        use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1;

        use crate::control::{ControlClient, Handled};

        use super::super::{
            is_would_block, PanelConfig, ShellHost, ShmBacking, BANNER_NAMESPACE, PANEL_NAMESPACE,
        };
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

        /// Panel presses toggle popups and grow the surface, all
        /// without Wayland attached (state only, no paint).
        #[test]
        fn presses_toggle_popup_and_resize_height() {
            use crate::popup::{PopupBody, POPUP_HEIGHT};
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            assert!(!host.popup.is_open());
            assert_eq!(host.popup_surface_height(), 32);
            // Clock press opens the calendar and grows the surface.
            host.press_panel(640, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            assert_eq!(host.popup_surface_height(), 32 + POPUP_HEIGHT);
            // Second press on the clock closes it again.
            host.press_panel(640, 16);
            assert!(!host.popup.is_open());
            assert_eq!(host.popup_surface_height(), 32);
            // Tile press opens that tile's menu; outside press closes.
            host.press_panel(1260, 16);
            assert!(host.popup.is_open());
            host.press_panel(4, 100);
            assert!(!host.popup.is_open());
        }

        /// Clock format write-back round trip through a fake backend:
        /// the shell toggle flips the shared key, and one tick
        /// re-renders the bar. Touches no real dconf: every backend
        /// pass goes through the fake, and paint reads only the
        /// snapshot.
        #[test]
        fn clock_toggle_flips_shared_key_and_rerenders_within_one_tick() {
            use crate::popup::{calendar_clock_row, popup_box, PopupBody};
            use crate::settings::{
                ClockFormat, MapBackend, SettingsBackend, CLOCK_FORMAT_KEY, INTERFACE_SCHEMA,
            };
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            let backend = MapBackend::with_values(&[(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY, "24h")]);
            host.tiles.refresh_with(&backend);
            assert_eq!(host.tiles.settings.clock_format, ClockFormat::TwentyFour);
            // Baseline paint records the pre-toggle key (no Wayland:
            // the key records without uploading, keeping the repaint
            // decision observable).
            host.paint_panel_surface(1280, 32);
            let before = host.panel_paint_key.clone().expect("baseline paint");
            // A press inside the open calendar but above the footer
            // row keeps it open and writes nothing.
            host.press_panel(640, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            let open_box = popup_box(
                &crate::popup::panel_layout(1280, 32, &host.tiles.clock),
                PopupBody::Calendar,
            );
            let footer = calendar_clock_row(&open_box);
            host.press_panel(open_box.x + 4, open_box.y + 4);
            assert!(footer.y > open_box.y + 4);
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            assert_eq!(
                backend
                    .string(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY)
                    .as_deref(),
                Some("24h")
            );
            host.close_popup();
            // The shell toggle flips the shared key...
            assert!(host.toggle_clock_format_with(&backend));
            assert_eq!(
                backend
                    .string(INTERFACE_SCHEMA, CLOCK_FORMAT_KEY)
                    .as_deref(),
                Some("12h")
            );
            // ...and one tick re-renders the bar on snapshot truth.
            host.tiles.refresh_with(&backend);
            host.paint_panel_surface(1280, 32);
            let after = host.panel_paint_key.clone().expect("repaint");
            assert_ne!(before, after);
            assert_eq!(after.clock_format, ClockFormat::Twelve);
            assert_eq!(after.clock, host.tiles.clock);
        }

        /// Shell-set wallpaper applies at once through the existing
        /// publish path: the picker write flips the shared key and
        /// the compositor drop file carries the new URI without
        /// waiting for the slow tick. Touches no real dconf and no
        /// real runtime dir: every backend pass goes through the
        /// fake, and the drop file lands in a temp dir.
        #[test]
        fn shell_set_wallpaper_applies_through_the_publish_path() {
            use crate::settings::{
                MapBackend, SettingsBackend, BACKGROUND_SCHEMA, PICTURE_URI_KEY,
            };
            use crate::tiles::TileSet;
            let (mut host, _dir) = test_host();
            let net = tempfile::tempdir().expect("tempdir");
            let power = tempfile::tempdir().expect("tempdir");
            let run = tempfile::tempdir().expect("tempdir");
            host.tiles = TileSet::with_roots("", net.path(), power.path(), run.path());
            let backend = MapBackend::default();
            assert!(host.set_wallpaper_uri_with("file:///wall.png", &backend));
            assert_eq!(
                backend
                    .string(BACKGROUND_SCHEMA, PICTURE_URI_KEY)
                    .as_deref(),
                Some("file:///wall.png")
            );
            assert_eq!(
                host.tiles.settings.wallpaper_uri.as_deref(),
                Some("file:///wall.png")
            );
            let drop = run.path().join(roost_compositor::wallpaper::WALLPAPER_FILE);
            assert_eq!(
                std::fs::read_to_string(&drop).expect("drop file"),
                "file:///wall.png"
            );
        }

        /// Wallpaper survives session restart: a fresh host re-reads
        /// the shared key and republishes the same drop file, so the
        /// compositor shows the same image after the shell restarts.
        #[test]
        fn wallpaper_survives_session_restart() {
            use crate::settings::{MapBackend, BACKGROUND_SCHEMA, PICTURE_URI_KEY};
            use crate::tiles::TileSet;
            let backend = MapBackend::with_values(&[(
                BACKGROUND_SCHEMA,
                PICTURE_URI_KEY,
                "file:///wall.png",
            )]);
            // Fresh session: default snapshot, nothing published yet,
            // and a fresh runtime dir with no drop file.
            let (mut host, _dir) = test_host();
            let net = tempfile::tempdir().expect("tempdir");
            let power = tempfile::tempdir().expect("tempdir");
            let run = tempfile::tempdir().expect("tempdir");
            host.tiles = TileSet::with_roots("", net.path(), power.path(), run.path());
            assert_eq!(host.tiles.settings.wallpaper_uri, None);
            // The slow tick's shape — re-read, then publish — restores
            // the same drop file the previous session wrote.
            host.tiles.refresh_with(&backend);
            host.publish_wallpaper();
            assert_eq!(
                host.tiles.settings.wallpaper_uri.as_deref(),
                Some("file:///wall.png")
            );
            let drop = run.path().join(roost_compositor::wallpaper::WALLPAPER_FILE);
            assert_eq!(
                std::fs::read_to_string(&drop).expect("drop file"),
                "file:///wall.png"
            );
        }

        /// Roost prefs surface round trip: a press on the calendar's
        /// prefs row flips the weekday prefix, the calendar stays
        /// open, and the pinned prefs file carries the flip without
        /// hand editing.
        #[test]
        fn calendar_prefs_press_flips_weekday_and_persists() {
            use crate::popup::{calendar_weekday_row, popup_box, PopupBody};
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            let state = tempfile::tempdir().expect("tempdir");
            let prefs = state.path().join(crate::prefs::PREFS_FILE);
            host.tiles.restore_prefs(&prefs);
            assert!(!host.tiles.prefs.clock_show_weekday);
            host.press_panel(640, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            let open_box = popup_box(
                &crate::popup::panel_layout(1280, 32, &host.tiles.clock),
                PopupBody::Calendar,
            );
            let row = calendar_weekday_row(&open_box);
            host.press_panel(row.x + 4, row.y + 4);
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            assert!(host.tiles.prefs.clock_show_weekday);
            assert!(host.tiles.clock.contains(' '));
            assert!(
                crate::prefs::load(&prefs).clock_show_weekday,
                "surface write reaches the prefs file"
            );
        }

        /// Changed Roost option shows the kept value after shell
        /// restart: a fresh host restoring from the same prefs file
        /// paints the weekday prefix with no further writes.
        #[test]
        fn changed_roost_option_shows_kept_value_after_restart() {
            let (mut host, _dir) = test_host();
            let state = tempfile::tempdir().expect("tempdir");
            let prefs = state.path().join(crate::prefs::PREFS_FILE);
            host.tiles.restore_prefs(&prefs);
            assert!(host.toggle_clock_weekday());
            assert!(host.tiles.prefs.clock_show_weekday);
            // Fresh session, same file: the kept value comes back.
            let (mut restarted, _dir) = test_host();
            assert!(!restarted.tiles.prefs.clock_show_weekday);
            restarted.tiles.restore_prefs(&prefs);
            assert!(restarted.tiles.prefs.clock_show_weekday);
            assert!(restarted.tiles.clock.contains(' '));
            // And flipping back persists too: no one-way latch.
            assert!(restarted.toggle_clock_weekday());
            assert!(!crate::prefs::load(&prefs).clock_show_weekday);
        }

        /// `close_popup` is a no-op when nothing is open and shrinks
        /// back when something is.
        #[test]
        fn close_popup_only_acts_when_open() {
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            host.close_popup();
            assert_eq!(host.popup_surface_height(), 32);
            host.press_panel(640, 16);
            assert!(host.popup.is_open());
            host.close_popup();
            assert!(!host.popup.is_open());
            assert_eq!(host.popup_surface_height(), 32);
        }

        /// Presses before the first configure are ignored: there is
        /// no arranged size to hit-test against yet.
        #[test]
        fn presses_need_an_arranged_size() {
            let (mut host, _dir) = test_host();
            assert_eq!(host.panel_size, None);
            host.press_panel(640, 16);
            assert!(!host.popup.is_open());
        }

        /// Dock presses with a launchable entry, without Wayland.
        fn dock_test_host() -> (ShellHost, tempfile::TempDir) {
            use crate::apps::AppEntry;
            let dir = tempfile::tempdir().expect("tempdir");
            let true_entry = AppEntry {
                app_id: "org.example.True.desktop".to_owned(),
                name: "True".to_owned(),
                generic_name: None,
                keywords: Vec::new(),
                argv: vec![std::ffi::OsString::from("/bin/true")],
                icon: None,
            };
            let mut host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(vec![true_entry]),
                Favorites::load(dir.path().join(crate::favorites::FAVORITES_FILE)),
            );
            host.favorites.pin("org.example.True.desktop");
            host.dock_size = Some((1280, crate::dock::DOCK_H));
            (host, dir)
        }

        /// Left press on a stopped pinned app launches it locally
        /// (`/bin/true`, reaped by the tracker) and arms nothing.
        #[test]
        fn dock_left_press_launches_stopped_app() {
            let (mut host, _dir) = dock_test_host();
            // Single item: slot 0 centers in 1280.
            host.press_dock(640, 28, crate::dock::BTN_LEFT);
            assert!(host.pending_dock.is_empty());
            let states = host.launcher.states();
            assert_eq!(states.len(), 1, "launch must track one app");
        }

        /// Right press toggles the pin and persists favorites. Note
        /// unpinning removes the slot, so re-pinning presses the
        /// running (now unpinned) item instead.
        #[test]
        fn dock_right_press_toggles_pin() {
            use crate::dock::BTN_RIGHT;
            let (mut host, _dir) = dock_test_host();
            assert!(host.favorites.contains("org.example.True.desktop"));
            host.press_dock(640, 28, BTN_RIGHT);
            assert!(!host.favorites.contains("org.example.True.desktop"));
            host.model.apply_window_list(
                vec![crate::model::WindowEntry::new(7, "True", false)
                    .with_app_id(Some("org.example.True".to_owned()))],
                vec![0],
            );
            host.press_dock(640, 28, BTN_RIGHT);
            assert!(host.favorites.contains("org.example.True.desktop"));
        }

        /// Switch and close arm the pending queue for the control
        /// loop instead of running inline.
        #[test]
        fn dock_switch_and_close_arm_pending() {
            use crate::dock::{DockAction, BTN_LEFT, BTN_MIDDLE};
            let (mut host, _dir) = dock_test_host();
            host.model.apply_window_list(
                vec![crate::model::WindowEntry::new(7, "True", true)
                    .with_app_id(Some("org.example.True".to_owned()))],
                vec![0],
            );
            host.press_dock(640, 28, BTN_LEFT);
            assert_eq!(host.take_dock_actions(), vec![DockAction::Switch(7)]);
            host.press_dock(640, 28, BTN_MIDDLE);
            assert_eq!(host.take_dock_actions(), vec![DockAction::Close(7)]);
            assert!(host.take_dock_actions().is_empty());
        }

        /// Evdev codes on a `us` layout for the focus keys.
        const EV_TAB: u32 = 15;
        const EV_F6: u32 = 64;
        const EV_D: u32 = 32;
        const EV_F1: u32 = 59;
        const EV_LEFT: u32 = 105;
        const EV_RIGHT: u32 = 106;
        const EV_UP: u32 = 103;
        const EV_DOWN: u32 = 108;
        const EV_RETURN: u32 = 28;
        const EV_ESCAPE: u32 = 1;
        const EV_SUPER: u32 = 125;
        const EV_ALT: u32 = 56;
        const EV_SHIFT: u32 = 42;

        /// Host with a live key feed: `on_key` resolves real keysyms.
        fn keyed_host() -> (ShellHost, tempfile::TempDir) {
            let (mut host, dir) = test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            (host, dir)
        }

        /// Feed one press through `on_key` and drain it into focus.
        fn press(host: &mut ShellHost, key: u32) {
            host.on_key(key, true);
            host.on_key(key, false);
            host.step_focus();
        }

        /// Tab walks the four panel stops left to right, then wraps.
        #[test]
        fn tab_walks_panel_stops_and_wraps() {
            use super::super::{FocusRegion, PANEL_STOPS};
            let (mut host, _dir) = keyed_host();
            assert_eq!(host.shell_focus(), None);
            for cursor in 0..PANEL_STOPS {
                press(&mut host, EV_TAB);
                assert_eq!(
                    host.shell_focus(),
                    Some(super::super::ShellFocus {
                        region: FocusRegion::Panel,
                        cursor,
                    }),
                    "tab {cursor}"
                );
            }
            press(&mut host, EV_TAB);
            assert_eq!(
                host.shell_focus().map(|focus| focus.cursor),
                Some(0),
                "tab wraps"
            );
        }

        /// Shift+Tab walks backward from no focus onto the last stop.
        #[test]
        fn shift_tab_walks_backward() {
            use super::super::{FocusRegion, PANEL_STOPS};
            let (mut host, _dir) = keyed_host();
            host.on_key(EV_SHIFT, true);
            host.on_key(EV_TAB, true);
            host.on_key(EV_TAB, false);
            host.on_key(EV_SHIFT, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus(),
                Some(super::super::ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: PANEL_STOPS - 1,
                })
            );
            // Backward steps keep walking down the strip.
            host.on_key(EV_SHIFT, true);
            press(&mut host, EV_TAB);
            host.on_key(EV_SHIFT, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus(),
                Some(super::super::ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: PANEL_STOPS - 2,
                })
            );
            let _ = FocusRegion::Dock;
        }

        /// F6 flips between strip and dock; arrows walk stops and
        /// cross regions vertically.
        #[test]
        fn f6_and_arrows_move_across_regions() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = dock_test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            assert!(!host.dock_items().is_empty(), "dock has a stop");
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                })
            );
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Dock,
                    cursor: 0,
                })
            );
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Panel)
            );
            // Right walks the strip; Down crosses to the dock; Up
            // crosses back, keeping the strip cursor.
            press(&mut host, EV_RIGHT);
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(1));
            press(&mut host, EV_DOWN);
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Dock)
            );
            press(&mut host, EV_UP);
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                })
            );
            press(&mut host, EV_LEFT);
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(3));
        }

        /// Super+D jumps to the dock, Alt+F1 to the panel, keeping
        /// the cursor when already in the region.
        #[test]
        fn super_d_and_alt_f1_jump_regions() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = dock_test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            host.on_key(EV_SUPER, true);
            press(&mut host, EV_D);
            host.on_key(EV_SUPER, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Dock,
                    cursor: 0,
                })
            );
            host.on_key(EV_ALT, true);
            press(&mut host, EV_F1);
            host.on_key(EV_ALT, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                })
            );
            // A Right move then a redundant Alt+F1 keeps the tile.
            press(&mut host, EV_RIGHT);
            host.on_key(EV_ALT, true);
            press(&mut host, EV_F1);
            host.on_key(EV_ALT, false);
            host.step_focus();
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(1));
            let _ = FocusRegion::Dock;
        }

        /// F6 never lands on an itemless dock: the cursor stays put.
        #[test]
        fn f6_skips_an_empty_dock() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = keyed_host();
            assert!(host.dock_items().is_empty(), "no dock stops");
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                })
            );
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                }),
                "empty dock keeps the strip cursor"
            );
            let _ = FocusRegion::Dock;
        }

        /// An emptied dock clears a stranded cursor instead of
        /// keeping focus on a stop that no longer exists.
        #[test]
        fn emptied_dock_clears_focus() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = dock_test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            press(&mut host, EV_F6);
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Dock)
            );
            host.favorites.unpin("org.example.True.desktop");
            let _ = host.favorites.save();
            assert!(host.dock_items().is_empty(), "unpin empties the dock");
            host.step_focus();
            assert_eq!(host.shell_focus(), None);
            let _ = ShellFocus {
                region: FocusRegion::Panel,
                cursor: 0,
            };
        }

        /// Enter on the focused clock opens the calendar and takes
        /// row focus, parking the strip cursor; rows walk with the
        /// arrows; Escape closes and restores the parked cursor.
        /// (Firing a footer row writes through Gio or the prefs
        /// file, so shell tests never fire calendar rows — the
        /// flip paths are covered by the tiles backend tests, and
        /// row activation end to end by the power-row lock test.)
        #[test]
        fn enter_on_clock_opens_calendar_with_row_focus() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::popup::PopupBody;
            let (mut host, _dir) = keyed_host();
            press(&mut host, EV_TAB);
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(0));
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(
                host.take_shell_submit(),
                "closed overview arms shell submit"
            );
            host.activate_focused();
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Popup,
                    cursor: 0,
                }),
                "row focus parks the strip cursor"
            );
            assert_eq!(
                host.return_stack,
                vec![ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                }],
                "strip cursor parked"
            );
            // Rows walk; the calendar stays open on row focus.
            host.on_key(EV_DOWN, true);
            host.on_key(EV_DOWN, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Popup,
                    cursor: 1,
                })
            );
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            // Escape closes and restores the parked strip cursor.
            host.on_key(EV_ESCAPE, true);
            host.on_key(EV_ESCAPE, false);
            assert_eq!(host.popup.body(), None);
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                }),
                "escape restores the parked cursor"
            );
        }

        /// Enter on a focused tile opens its menu, like a press.
        #[test]
        fn enter_on_tile_opens_menu() {
            use crate::popup::PopupBody;
            let (mut host, _dir) = keyed_host();
            press(&mut host, EV_TAB);
            press(&mut host, EV_TAB);
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(1));
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
            host.activate_focused();
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(0)));
        }

        /// Enter on a focused dock slot fires the left-press action:
        /// Switch arms the pending queue for the control loop.
        #[test]
        fn enter_on_dock_slot_switches() {
            use super::super::FocusRegion;
            use crate::dock::DockAction;
            let (mut host, _dir) = dock_test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            host.model.apply_window_list(
                vec![crate::model::WindowEntry::new(7, "True", true)
                    .with_app_id(Some("org.example.True".to_owned()))],
                vec![0],
            );
            press(&mut host, EV_F6);
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Dock)
            );
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
            host.activate_focused();
            assert_eq!(host.take_dock_actions(), vec![DockAction::Switch(7)]);
        }

        /// Enter on a pinned-but-missing slot is a quiet no-op: no
        /// launch, no armed action, nothing spawned.
        #[test]
        fn enter_on_missing_slot_is_quiet() {
            use super::super::FocusRegion;
            let (mut host, _dir) = test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            host.favorites.pin("org.example.Missing.desktop");
            let _ = host.favorites.save();
            assert_eq!(host.dock_items().len(), 1, "missing pin still a stop");
            press(&mut host, EV_F6);
            press(&mut host, EV_F6);
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Dock)
            );
            host.activate_focused();
            assert!(host.take_dock_actions().is_empty());
            assert_eq!(host.popup.body(), None);
        }

        /// Byte sampler for the ring tests.
        fn pixel_at(pixels: &[u8], width: i32, x: i32, y: i32) -> [u8; 4] {
            let start = (y as usize * width as usize + x as usize) * 4;
            pixels[start..start + 4].try_into().expect("in bounds")
        }

        /// Every panel stop paints its accent ring; without focus the
        /// same pixels are ring-free.
        #[test]
        fn panel_stops_paint_focus_rings() {
            use super::super::{FocusRegion, ShellFocus, PANEL_STOPS};
            use crate::overview::ACCENT;
            use crate::popup::panel_layout;
            let (mut host, _dir) = test_host();
            let width = 1280;
            let strip_h = host.panel.height as i32;
            let plain = host.render_panel_pixels(width, strip_h);
            for cursor in 0..PANEL_STOPS {
                host.focus = Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor,
                });
                let painted = host.render_panel_pixels(width, strip_h);
                let layout = panel_layout(width, strip_h, &host.tiles.clock);
                let stop = if cursor == 0 {
                    layout.clock
                } else {
                    layout.tiles[cursor - 1]
                };
                // Left ring edge, vertically centered on the stop.
                let (x, y) = (stop.x - 1, stop.y + stop.h / 2);
                assert_eq!(
                    pixel_at(&painted, width, x, y),
                    ACCENT,
                    "ring on stop {cursor}"
                );
                assert_ne!(
                    pixel_at(&plain, width, x, y),
                    ACCENT,
                    "no ring without focus on stop {cursor}"
                );
            }
        }

        /// The focused dock slot paints its accent ring; without
        /// focus the same pixel is ring-free.
        #[test]
        fn dock_slot_paints_focus_ring() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::dock::dock_slot_at;
            use crate::overview::ACCENT;
            let (mut host, _dir) = dock_test_host();
            let width = 1280;
            let height = crate::dock::DOCK_H;
            let items = host.dock_items();
            assert_eq!(items.len(), 1);
            let plain = host.render_dock_pixels(width, height, &items);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Dock,
                cursor: 0,
            });
            let painted = host.render_dock_pixels(width, height, &items);
            let (x0, y0, _, h) = dock_slot_at(width, 0, items.len(), 0);
            let (x, y) = (x0 - 1, y0 + h / 2);
            assert_eq!(pixel_at(&painted, width, x, y), ACCENT, "ring on slot");
            assert_ne!(
                pixel_at(&plain, width, x, y),
                ACCENT,
                "no ring without focus"
            );
        }

        /// Paint keys carry the cursor: moving focus repaints both
        /// surfaces by construction.
        #[test]
        fn paint_keys_carry_focus() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = dock_test_host();
            let width = 1280;
            let items = host.dock_items();
            let panel_plain = host.panel_render_key(width, 32);
            let dock_plain = host.dock_render_key(width, crate::dock::DOCK_H, &items);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Panel,
                cursor: 2,
            });
            let panel_focused = host.panel_render_key(width, 32);
            let dock_focused = host.dock_render_key(width, crate::dock::DOCK_H, &items);
            assert_ne!(panel_plain, panel_focused, "panel key moves with focus");
            assert_ne!(dock_plain, dock_focused, "dock key moves with focus");
            host.focus = None;
            assert_eq!(
                host.panel_render_key(width, 32),
                panel_plain,
                "key restores"
            );
            assert_eq!(
                host.dock_render_key(width, crate::dock::DOCK_H, &items),
                dock_plain,
                "dock key restores"
            );
        }

        /// Three injected hits behind the cursor (titles only; the
        /// hub stays quiet so the count is exact).
        fn three_hits() -> Vec<crate::search::SearchResult> {
            use crate::search::{SearchAction, SearchResult};
            ["alpha", "beta", "gamma"]
                .into_iter()
                .enumerate()
                .map(|(i, title)| SearchResult {
                    title: title.to_owned(),
                    app_id: None,
                    action: SearchAction::Focus { window: i as u64 },
                })
                .collect()
        }

        /// Popup rows walk with Up/Down and wrap inside the open
        /// menu's row count.
        #[test]
        fn popup_rows_move_and_wrap() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::popup::PopupBody;
            let (mut host, _dir) = keyed_host();
            // Two Tabs reach the network tile; Enter opens its menu
            // and takes row focus.
            press(&mut host, EV_TAB);
            press(&mut host, EV_TAB);
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
            host.activate_focused();
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(0)));
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Popup,
                    cursor: 0,
                })
            );
            // Two network rows: Down, Down wraps home.
            host.on_key(EV_DOWN, true);
            host.on_key(EV_DOWN, false);
            host.step_focus();
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(1));
            host.on_key(EV_DOWN, true);
            host.on_key(EV_DOWN, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus().map(|focus| focus.cursor),
                Some(0),
                "rows wrap"
            );
            host.on_key(EV_UP, true);
            host.on_key(EV_UP, false);
            host.step_focus();
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(1));
        }

        /// Enter on the power row arms the lock request and keeps
        /// the menu open, like a press.
        #[test]
        fn enter_on_power_row_arms_lock() {
            use crate::popup::PopupBody;
            let (mut host, _dir) = keyed_host();
            press(&mut host, EV_TAB);
            press(&mut host, EV_TAB);
            press(&mut host, EV_TAB);
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(2));
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
            host.activate_focused();
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(1)));
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
            host.activate_focused();
            assert!(host.take_lock(), "lock row arms the driver");
            assert_eq!(
                host.popup.body(),
                Some(PopupBody::Menu(1)),
                "toggle menus stay open"
            );
        }

        /// Escape splits in two: with a popup over the overview the
        /// popup closes and dismissal stays disarmed; the next
        /// Escape dismisses the overview onto the focused window.
        #[test]
        fn escape_splits_popup_then_overview() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::popup::PopupBody;
            let (mut host, _dir) = keyed_host();
            host.model.set_overview_open(true);
            host.popup.open(PopupBody::Calendar);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Popup,
                cursor: 1,
            });
            host.return_stack = vec![ShellFocus {
                region: FocusRegion::Panel,
                cursor: 2,
            }];
            host.on_key(EV_ESCAPE, true);
            host.on_key(EV_ESCAPE, false);
            assert_eq!(host.popup.body(), None, "first escape closes the popup");
            assert!(
                !host.take_dismiss(),
                "overview stays open after the first escape"
            );
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 2,
                }),
                "parked cursor restored"
            );
            host.on_key(EV_ESCAPE, true);
            host.on_key(EV_ESCAPE, false);
            assert!(host.take_dismiss(), "second escape dismisses the overview");
        }

        /// With the overview closed, Escape on an open popup only
        /// closes the popup — no submit or dismissal arms.
        #[test]
        fn escape_closed_overview_closes_popup_only() {
            use crate::popup::PopupBody;
            let (mut host, _dir) = keyed_host();
            host.popup.open(PopupBody::Calendar);
            host.on_key(EV_ESCAPE, true);
            host.on_key(EV_ESCAPE, false);
            assert_eq!(host.popup.body(), None);
            assert!(!host.take_dismiss());
            assert!(!host.take_submit());
            assert!(!host.take_shell_submit());
        }

        /// Overview arrows walk the collected hits with wrap, and
        /// Enter takes the focused row (or the top hit from any
        /// other region).
        #[test]
        fn overview_cursor_moves_over_hits() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = keyed_host();
            host.model.set_overview_open(true);
            host.overview_hits = three_hits();
            host.on_key(EV_DOWN, true);
            host.on_key(EV_DOWN, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Overview,
                    cursor: 0,
                })
            );
            // Down walks 1, 2, then wraps to 0; Up walks back.
            for cursor in [1, 2, 0] {
                host.on_key(EV_DOWN, true);
                host.on_key(EV_DOWN, false);
                host.step_focus();
                assert_eq!(
                    host.shell_focus().map(|focus| focus.cursor),
                    Some(cursor),
                    "walk"
                );
            }
            host.on_key(EV_UP, true);
            host.on_key(EV_UP, false);
            host.step_focus();
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(2));
            let _ = FocusRegion::Panel;
            host.focus = Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor: 1,
            });
            assert_eq!(
                host.take_overview_hit().map(|hit| hit.title),
                Some("beta".to_owned()),
                "enter takes the focused row"
            );
            host.focus = Some(ShellFocus {
                region: FocusRegion::Panel,
                cursor: 0,
            });
            assert_eq!(
                host.take_overview_hit().map(|hit| hit.title),
                Some("alpha".to_owned()),
                "other regions take the top hit"
            );
        }

        /// Typing a re-query clears the cached hits and restarts the
        /// result cursor; other regions keep their cursor.
        #[test]
        fn requery_restarts_result_cursor() {
            use super::super::{FocusRegion, ShellFocus};
            const EV_A: u32 = 30;
            let (mut host, _dir) = keyed_host();
            host.model.set_overview_open(true);
            host.overview_hits = three_hits();
            host.focus = Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor: 2,
            });
            host.on_key(EV_A, true);
            host.on_key(EV_A, false);
            assert!(host.overview_hits.is_empty(), "re-query drops cached hits");
            assert_eq!(
                host.shell_focus().map(|focus| focus.cursor),
                Some(0),
                "cursor restarts"
            );
            assert_eq!(host.search_text(), "a");
        }

        /// The overview paint key carries the cursor: moving result
        /// focus repaints by construction.
        #[test]
        fn overview_key_carries_focus() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = test_host();
            let plain = host.overview_render_key(None, 1280, 800);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor: 1,
            });
            assert_ne!(
                host.overview_render_key(None, 1280, 800),
                plain,
                "overview key moves with focus"
            );
        }

        /// Trap: closing the popup restores the parked cursor, and
        /// with an empty stack the cursor clears.
        #[test]
        fn trap_popup_close_restores_or_clears() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::popup::PopupBody;
            let (mut host, _dir) = dock_test_host();
            host.popup.open(PopupBody::Calendar);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Popup,
                cursor: 1,
            });
            host.return_stack = vec![ShellFocus {
                region: FocusRegion::Dock,
                cursor: 0,
            }];
            host.close_popup();
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Dock,
                    cursor: 0,
                }),
                "parked dock cursor restored"
            );
            // No stack, dead surface: the cursor clears instead of
            // sticking.
            host.popup.open(PopupBody::Calendar);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Popup,
                cursor: 0,
            });
            host.return_stack.clear();
            host.close_popup();
            assert_eq!(host.shell_focus(), None);
        }

        /// Trap: a dead overview cursor falls back through the stack
        /// to the last live region, draining dead tops with it.
        #[test]
        fn trap_overview_close_falls_back_to_live() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = keyed_host();
            host.focus = Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor: 3,
            });
            host.return_stack = vec![ShellFocus {
                region: FocusRegion::Panel,
                cursor: 2,
            }];
            assert!(!host.model.is_overview_open());
            host.settle_focus();
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 2,
                }),
                "dead overview cursor restores the parked stop"
            );
            assert!(host.return_stack.is_empty());
            // Nothing parked: the cursor clears.
            host.focus = Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor: 0,
            });
            host.settle_focus();
            assert_eq!(host.shell_focus(), None);
            // Dead stack tops drain under a live cursor.
            host.focus = Some(ShellFocus {
                region: FocusRegion::Panel,
                cursor: 0,
            });
            host.return_stack = vec![
                ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 1,
                },
                ShellFocus {
                    region: FocusRegion::Popup,
                    cursor: 0,
                },
            ];
            host.settle_focus();
            assert_eq!(
                host.return_stack,
                vec![ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 1,
                }],
                "dead popup top drains"
            );
        }

        /// Focused menu rows paint their accent ring; without focus
        /// the same pixels are ring-free.
        #[test]
        fn popup_row_paints_focus_ring() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::overview::ACCENT;
            use crate::popup::{panel_layout, popup_box, tile_row_rect, PopupBody, POPUP_HEIGHT};
            let (mut host, _dir) = test_host();
            let width = 1280;
            let strip_h = host.panel.height as i32;
            let height = strip_h + POPUP_HEIGHT;
            host.popup.open(PopupBody::Menu(0));
            let plain = host.render_panel_pixels(width, height);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Popup,
                cursor: 1,
            });
            let painted = host.render_panel_pixels(width, height);
            let layout = panel_layout(width, strip_h, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::Menu(0));
            let row = tile_row_rect(&open_box, 1, 2).expect("row 1 box");
            let (x, y) = (row.x - 1, row.y + 2);
            assert_eq!(pixel_at(&painted, width, x, y), ACCENT, "ring on row");
            assert_ne!(
                pixel_at(&plain, width, x, y),
                ACCENT,
                "no ring without focus"
            );
        }

        /// Indicator cells append to the strip stops: four Tabs
        /// cross clock and tiles, the fifth lands on the hosted
        /// cell, and Enter runs the shared strip-press call (empty
        /// menus activate in place with no popup).
        #[test]
        fn indicator_stops_walk_and_activate() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = indicator_test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            assert_eq!(host.panel_stop_count(), 5);
            for cursor in 0..5 {
                press(&mut host, EV_TAB);
                assert_eq!(
                    host.shell_focus().map(|focus| focus.cursor),
                    Some(cursor),
                    "tab {cursor}"
                );
            }
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Panel)
            );
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
            host.activate_focused();
            assert!(!host.popup.is_open(), "empty menu activates in place");
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 4,
                }),
                "indicator stop keeps focus"
            );
            assert_eq!(host.indicators.items().len(), 1, "item stays hosted");
            let _ = FocusRegion::Dock;
        }

        /// Seeded indicator menus walk and fire by keyboard: rows
        /// move, Enter sends the enabled row and dismisses, and the
        /// parked strip cursor comes back.
        #[test]
        fn indicator_menu_key_rows_fire_and_restore() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::popup::PopupBody;
            use crate::watcher::MenuEntry;
            let (mut host, _dir) = indicator_test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            if let Some(mut item) = host.indicators.get("test.indicator").cloned() {
                item.menu = vec![
                    MenuEntry {
                        id: 7,
                        label: "open".to_owned(),
                        enabled: true,
                    },
                    MenuEntry {
                        id: 8,
                        label: "quit".to_owned(),
                        enabled: false,
                    },
                ];
                host.indicators.upsert(item);
            }
            host.popup.open(PopupBody::IndicatorMenu(0));
            host.focus = Some(ShellFocus {
                region: FocusRegion::Popup,
                cursor: 0,
            });
            host.return_stack = vec![ShellFocus {
                region: FocusRegion::Panel,
                cursor: 4,
            }];
            assert_eq!(host.popup_row_count(), 2);
            host.on_key(EV_DOWN, true);
            host.on_key(EV_DOWN, false);
            host.step_focus();
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(1));
            host.on_key(EV_UP, true);
            host.on_key(EV_UP, false);
            host.step_focus();
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(0));
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
            host.activate_focused();
            assert_eq!(host.popup.body(), None, "row fire dismisses");
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 4,
                }),
                "parked indicator stop restored"
            );
        }

        /// Unhosting the focused cell settles the cursor onto the
        /// last tile instead of stranding it past the strip.
        #[test]
        fn unhosted_indicator_settles_cursor() {
            use super::super::{FocusRegion, ShellFocus};
            let (mut host, _dir) = indicator_test_host();
            host.focus = Some(ShellFocus {
                region: FocusRegion::Panel,
                cursor: 4,
            });
            assert!(host.indicators.remove("test.indicator"));
            assert_eq!(host.panel_stop_count(), 4);
            host.settle_focus();
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 3,
                }),
                "cursor clamps into the live stops"
            );
        }

        /// The focused indicator cell paints its accent ring.
        #[test]
        fn indicator_stop_paints_focus_ring() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::overview::ACCENT;
            let (mut host, _dir) = indicator_test_host();
            let width = 1280;
            let strip_h = host.panel.height as i32;
            host.focus = Some(ShellFocus {
                region: FocusRegion::Panel,
                cursor: 4,
            });
            let painted = host.render_panel_pixels(width, strip_h);
            let cells = crate::watcher::indicator_cells(
                crate::watcher::indicator_right_x(width),
                strip_h,
                1,
            );
            let cell = cells[0];
            assert_eq!(
                pixel_at(&painted, width, cell.x - 1, cell.y + cell.h / 2),
                ACCENT,
                "ring on the indicator cell"
            );
        }

        /// Headline key-only run, no pointer: open the calendar,
        /// launch a dock app, and activate an indicator, all
        /// through `on_key` plus the run-loop drains.
        #[test]
        fn key_only_run_opens_launches_and_activates() {
            use super::super::{FocusRegion, ShellFocus};
            use crate::popup::PopupBody;

            // Flow 1 — calendar: Tab to the clock, Enter opens,
            // Down walks a footer row, Escape closes and restores.
            let (mut host, _dir) = keyed_host();
            press(&mut host, EV_TAB);
            submit(&mut host);
            host.activate_focused();
            assert_eq!(host.popup.body(), Some(PopupBody::Calendar));
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Popup)
            );
            press(&mut host, EV_DOWN);
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(1));
            host.on_key(EV_ESCAPE, true);
            host.on_key(EV_ESCAPE, false);
            assert_eq!(host.popup.body(), None);
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 0,
                })
            );

            // Flow 2 — dock launch: Super+D to the first slot,
            // Enter launches the pinned app through the tracker.
            let (mut host, _dir) = dock_test_host();
            use crate::keyboard::XkbFeed;
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            host.on_key(EV_SUPER, true);
            press(&mut host, EV_D);
            host.on_key(EV_SUPER, false);
            host.step_focus();
            assert_eq!(
                host.shell_focus().map(|focus| focus.region),
                Some(FocusRegion::Dock)
            );
            submit(&mut host);
            host.activate_focused();
            assert_eq!(host.launcher.len(), 1, "pinned app spawned");
            assert!(host.take_dock_actions().is_empty());

            // Flow 3 — indicator: Tabs to the hosted cell, Enter
            // activates in place with no popup.
            let (mut host, _dir) = indicator_test_host();
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
            for _ in 0..5 {
                press(&mut host, EV_TAB);
            }
            assert_eq!(host.shell_focus().map(|focus| focus.cursor), Some(4));
            submit(&mut host);
            host.activate_focused();
            assert!(!host.popup.is_open());
            assert_eq!(host.indicators.items().len(), 1);
        }

        /// Feed one closed-overview Enter through `on_key` and drain
        /// it into the shell-submit flag, like the run loop.
        fn submit(host: &mut ShellHost) {
            host.on_key(EV_RETURN, true);
            host.on_key(EV_RETURN, false);
            assert!(host.take_shell_submit());
        }

        /// Dock presses before the first configure are ignored.
        #[test]
        fn dock_presses_need_an_arranged_size() {
            use crate::dock::BTN_LEFT;
            let (mut host, _dir) = dock_test_host();
            host.dock_size = None;
            host.press_dock(640, 28, BTN_LEFT);
            assert!(host.pending_dock.is_empty());
            assert!(host.launcher.states().is_empty());
        }

        /// Stack fixture: a pinned `stack:<dir>` favorite backed by
        /// one launchable desktop file plus one plain file.
        fn stack_test_host() -> (ShellHost, tempfile::TempDir) {
            use crate::apps::AppEntry;
            let dir = tempfile::tempdir().expect("tempdir");
            let stack = dir.path().join("stack");
            std::fs::create_dir_all(&stack).expect("stack dir");
            std::fs::write(
                stack.join("tool.desktop"),
                "[Desktop Entry]\nName=Tool\nExec=/bin/true\nType=Application\n",
            )
            .expect("stack desktop file");
            std::fs::write(stack.join("notes.txt"), "plain file cell").expect("stack plain file");
            let mut host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(vec![AppEntry {
                    app_id: "org.example.True.desktop".to_owned(),
                    name: "True".to_owned(),
                    generic_name: None,
                    keywords: Vec::new(),
                    argv: vec![std::ffi::OsString::from("/bin/true")],
                    icon: None,
                }]),
                Favorites::load(dir.path().join(crate::favorites::FAVORITES_FILE)),
            );
            let fav = format!("{}{}", crate::dock::STACK_PREFIX, stack.display());
            assert!(host.favorites.pin(&fav), "stack favorite must pin");
            host.dock_size = Some((1280, crate::dock::DOCK_H));
            (host, dir)
        }

        /// Left press on a stack item opens the grid and grows the
        /// surface; a second press on the item closes it again.
        #[test]
        fn stack_press_opens_grid_and_grows_surface() {
            use crate::dock::{BTN_LEFT, DOCK_H, STACK_GRID_H};
            let (mut host, _dir) = stack_test_host();
            assert_eq!(host.dock_surface_height(), DOCK_H);
            host.press_dock(640, 28, BTN_LEFT);
            assert_eq!(host.open_stack, Some(0));
            assert_eq!(host.stack_cache.len(), 2);
            assert_eq!(host.dock_surface_height(), DOCK_H + STACK_GRID_H);
            // Apps sort first: cell zero launches.
            assert!(host.stack_cache[0].app);
            // Second press on the same (shifted-down) slot toggles.
            host.press_dock(640, STACK_GRID_H + 28, BTN_LEFT);
            assert_eq!(host.open_stack, None);
            assert_eq!(host.dock_surface_height(), DOCK_H);
        }

        /// Left press on a grid cell launches it (`/bin/true`,
        /// tracked) and closes the grid.
        #[test]
        fn stack_cell_press_launches_and_closes() {
            use crate::dock::{stack_cell_origin, BTN_LEFT};
            let (mut host, _dir) = stack_test_host();
            host.press_dock(640, 28, BTN_LEFT);
            assert_eq!(host.open_stack, Some(0));
            let (cx, cy) = stack_cell_origin(1280, 0);
            host.press_dock(cx + 10, cy + 10, BTN_LEFT);
            assert_eq!(host.open_stack, None);
            assert_eq!(host.launcher.states().len(), 1);
            // A file cell opens through the handler and also closes.
            host.press_dock(640, 28, BTN_LEFT);
            let (fx, fy) = stack_cell_origin(1280, 1);
            host.press_dock(fx + 10, fy + 10, BTN_LEFT);
            assert_eq!(host.open_stack, None);
        }

        /// Unpinning the open stack closes the grid with it.
        #[test]
        fn stack_unpin_closes_grid() {
            use crate::dock::{BTN_LEFT, BTN_RIGHT, STACK_GRID_H};
            let (mut host, _dir) = stack_test_host();
            host.press_dock(640, 28, BTN_LEFT);
            assert_eq!(host.open_stack, Some(0));
            host.press_dock(640, STACK_GRID_H + 28, BTN_RIGHT);
            assert_eq!(host.open_stack, None);
            assert!(host.dock_items().is_empty());
        }

        /// Indicator fixture: arranged panel size, fixed clock, one
        /// hosted item, and no bus behind the watcher (disconnected in
        /// tests, so menu fetches read empty).
        fn indicator_test_host() -> (ShellHost, tempfile::TempDir) {
            use crate::watcher::{IndicatorIcon, IndicatorItem};
            let (mut host, dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            host.indicators.upsert(IndicatorItem {
                service: "test.indicator".to_owned(),
                title: "Test".to_owned(),
                icon: IndicatorIcon::Named("test".to_owned()),
                menu: Vec::new(),
            });
            (host, dir)
        }

        /// Cell press with an empty menu activates directly: no popup
        /// opens (nothing to show) and the item stays hosted.
        #[test]
        fn indicator_press_with_empty_menu_activates_and_dismisses() {
            let (mut host, _dir) = indicator_test_host();
            assert_eq!(host.indicators.items().len(), 1);
            let cells =
                crate::watcher::indicator_cells(crate::watcher::indicator_right_x(1280), 32, 1);
            let cell = cells[0];
            host.press_panel(cell.x + cell.w / 2, cell.y + cell.h / 2);
            assert!(!host.popup.is_open());
            assert_eq!(host.indicators.items().len(), 1);
        }

        /// Row press inside an open indicator menu dismisses it, for
        /// enabled and disabled rows alike.
        #[test]
        fn indicator_menu_row_press_fires_and_dismisses() {
            use crate::popup::{panel_layout, popup_box, PopupBody};
            use crate::watcher::MenuEntry;
            let (mut host, _dir) = indicator_test_host();
            if let Some(mut item) = host.indicators.get("test.indicator").cloned() {
                item.menu = vec![
                    MenuEntry {
                        id: 7,
                        label: "open".to_owned(),
                        enabled: true,
                    },
                    MenuEntry {
                        id: 8,
                        label: "quit".to_owned(),
                        enabled: false,
                    },
                ];
                host.indicators.upsert(item);
            }
            host.popup.open(PopupBody::IndicatorMenu(0));
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::IndicatorMenu(0));
            // First-row center: inside the box, on row 0.
            host.press_panel(open_box.x + open_box.w / 2, open_box.y + 10 + 12);
            assert!(!host.popup.is_open(), "menu row press dismisses");
            // Reopen and press the disabled second row: still dismisses.
            host.popup.open(PopupBody::IndicatorMenu(0));
            host.press_panel(open_box.x + open_box.w / 2, open_box.y + 10 + 24 + 12);
            assert!(!host.popup.is_open(), "disabled row press dismisses");
        }

        /// Row press inside the open sound menu with no bus fires
        /// nothing: no toggle arms, the menu stays open, and the tile
        /// keeps its sysfs state.
        #[test]
        fn sound_menu_row_press_without_bus_keeps_menu_open() {
            use crate::popup::{panel_layout, popup_box, tile_row_at, PopupBody, TILE_ROWS_TOP};
            use crate::tiles::SOUND_TILE_INDEX;
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            // Sound tile press opens the sound menu (strip order:
            // network, power, sound).
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let sound_tile = layout.tiles[SOUND_TILE_INDEX];
            host.press_panel(sound_tile.x + sound_tile.w / 2, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(SOUND_TILE_INDEX)));
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::Menu(SOUND_TILE_INDEX));
            // First-row center: inside the box, on row 0.
            let y = open_box.y + TILE_ROWS_TOP + 2;
            assert_eq!(tile_row_at(&open_box, y, 3), Some(0));
            host.press_panel(open_box.x + open_box.w / 2, y);
            assert_eq!(
                host.popup.body(),
                Some(PopupBody::Menu(SOUND_TILE_INDEX)),
                "no-bus row press keeps the menu open"
            );
            assert_eq!(host.tiles.sound_pending(), None);
        }

        /// Row press inside the open network menu with no bus fires
        /// nothing: no toggle arms, the menu stays open, and the tile
        /// keeps its sysfs state.
        #[test]
        fn network_menu_row_press_without_bus_keeps_menu_open() {
            use crate::popup::{panel_layout, popup_box, tile_row_at, PopupBody, TILE_ROWS_TOP};
            use crate::tiles::NETWORK_TILE_INDEX;
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            // Rightmost tile press opens the network menu.
            host.press_panel(1260, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(NETWORK_TILE_INDEX)));
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::Menu(NETWORK_TILE_INDEX));
            // First-row center: inside the box, on row 0.
            let y = open_box.y + TILE_ROWS_TOP + 2;
            assert_eq!(tile_row_at(&open_box, y, 2), Some(0));
            host.press_panel(open_box.x + open_box.w / 2, y);
            assert_eq!(
                host.popup.body(),
                Some(PopupBody::Menu(NETWORK_TILE_INDEX)),
                "no-bus row press keeps the menu open"
            );
            assert_eq!(host.tiles.network_pending(), None);
        }

        /// Row presses across every row of both menus with no bus
        /// fire nothing: each disabled row is skipped, both menus
        /// stay open, and no toggle arms.
        #[test]
        fn menu_row_presses_without_bus_skip_every_disabled_row() {
            use crate::popup::{panel_layout, popup_box, PopupBody, TILE_ROWS_TOP, TILE_ROW_H};
            use crate::tiles::{NETWORK_TILE_INDEX, SOUND_TILE_INDEX};
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();

            // Network menu: press every toggle row.
            host.press_panel(1260, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(NETWORK_TILE_INDEX)));
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::Menu(NETWORK_TILE_INDEX));
            for row in 0..2 {
                let y = open_box.y + TILE_ROWS_TOP + row * TILE_ROW_H + 2;
                host.press_panel(open_box.x + open_box.w / 2, y);
                assert_eq!(
                    host.popup.body(),
                    Some(PopupBody::Menu(NETWORK_TILE_INDEX)),
                    "disabled network row {row} skips"
                );
            }
            assert_eq!(host.tiles.network_pending(), None);
            host.close_popup();

            // Sound menu: press every toggle row.
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let sound_tile = layout.tiles[SOUND_TILE_INDEX];
            host.press_panel(sound_tile.x + sound_tile.w / 2, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(SOUND_TILE_INDEX)));
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::Menu(SOUND_TILE_INDEX));
            for row in 0..3 {
                let y = open_box.y + TILE_ROWS_TOP + row * TILE_ROW_H + 2;
                host.press_panel(open_box.x + open_box.w / 2, y);
                assert_eq!(
                    host.popup.body(),
                    Some(PopupBody::Menu(SOUND_TILE_INDEX)),
                    "disabled sound row {row} skips"
                );
            }
            assert_eq!(host.tiles.sound_pending(), None);
        }

        /// Power menu press on the lock row arms a manual lock
        /// request: the row paints enabled, the press arms it, and
        /// the drain consumes it exactly once.
        #[test]
        fn power_menu_lock_row_press_arms_manual_lock() {
            use crate::popup::{lock_rows, panel_layout, popup_box, PopupBody, TILE_ROWS_TOP};
            use crate::tiles::POWER_TILE_INDEX;
            assert_eq!(lock_rows().len(), 1, "single lock row");
            assert!(lock_rows()[0].enabled, "lock row always fires");
            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let power_tile = layout.tiles[POWER_TILE_INDEX];
            host.press_panel(power_tile.x + power_tile.w / 2, 16);
            assert_eq!(host.popup.body(), Some(PopupBody::Menu(POWER_TILE_INDEX)));
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::Menu(POWER_TILE_INDEX));
            let y = open_box.y + TILE_ROWS_TOP + 2;
            assert!(!host.take_lock(), "nothing armed before the press");
            host.press_panel(open_box.x + open_box.w / 2, y);
            assert!(host.take_lock(), "lock row press arms the request");
            assert!(!host.take_lock(), "drain consumes it once");
        }

        /// Armed lock request leaves over the control channel through
        /// the run-loop driver: press arms, the driver sends Lock, and
        /// the hub applies it to the locked snapshot.
        #[test]
        fn armed_lock_request_drives_lock_command() {
            use crate::popup::{panel_layout, popup_box, PopupBody, TILE_ROWS_TOP};
            use crate::tiles::POWER_TILE_INDEX;
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("control.sock");
            let mut model = StateModel::new();
            let mut hub =
                ControlHub::bind(socket_path.clone(), Rc::new(TokenStore::new()), SEAT_NAME)
                    .unwrap();
            let (mut host, _favdir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let power_tile = layout.tiles[POWER_TILE_INDEX];
            host.press_panel(power_tile.x + power_tile.w / 2, 16);
            let layout = panel_layout(1280, 32, &host.tiles.clock);
            let open_box = popup_box(&layout, PopupBody::Menu(POWER_TILE_INDEX));
            host.press_panel(open_box.x + open_box.w / 2, open_box.y + TILE_ROWS_TOP + 2);
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
            super::drive_lock_actions(&mut host, &mut client);
            let mut locked_seen = false;
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Snapshot { .. }) => {
                        if client.locked() {
                            locked_seen = true;
                        }
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("lock poll failed: {e}"),
                }
                if locked_seen {
                    break;
                }
            }
            assert!(locked_seen, "armed press drives Lock to the hub");
        }

        /// `paint_panel_surface` carries indicator pixels: the shm
        /// backing painted with an item hosted differs from the bare
        /// strip and paints strictly more glyph pixels.
        #[test]
        fn paint_panel_surface_includes_indicator_pixels() {
            use crate::overview::{BG, BYTES_PER_PIXEL};
            use crate::watcher::{IndicatorIcon, IndicatorItem};
            let mut comp = TestCompositor::new();
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();
            let mut queue = conn.new_event_queue();
            let (mut host, _dir) = test_host();
            attach_host(&mut comp, &conn, &mut queue, &mut host);
            let wayland = host.wayland.clone().expect("attached");
            host.surface = Some(wayland.compositor.create_surface(&wayland.qh, ()));
            host.tiles.clock = "12:34".to_owned();

            fn backing_pixels(host: &mut ShellHost) -> Vec<u8> {
                use std::io::{Read, Seek, SeekFrom};
                let backing = host.panel_backing.as_mut().expect("panel painted");
                backing._file.seek(SeekFrom::Start(0)).expect("rewind");
                let mut out = Vec::new();
                backing._file.read_to_end(&mut out).expect("read");
                out
            }

            fn non_bg(pixels: &[u8]) -> usize {
                let (chunks, _) = pixels.as_chunks::<BYTES_PER_PIXEL>();
                chunks.iter().filter(|pixel| **pixel != BG).count()
            }

            host.paint_panel_surface(800, 32);
            let plain = backing_pixels(&mut host);
            assert_eq!(plain.len(), 800 * 32 * BYTES_PER_PIXEL);

            host.indicators.upsert(IndicatorItem {
                service: "test.indicator".to_owned(),
                title: "Test".to_owned(),
                icon: IndicatorIcon::Named("test".to_owned()),
                menu: Vec::new(),
            });
            host.panel_backing = None;
            host.panel_paint_key = None;
            host.panel_size = None;
            host.paint_panel_surface(800, 32);
            let with_indicator = backing_pixels(&mut host);
            assert_ne!(
                plain, with_indicator,
                "indicator pixels reach the panel surface"
            );
            assert!(non_bg(&with_indicator) > non_bg(&plain));
        }

        /// Overview open paints window pixels: opening the overview
        /// against the live in-process compositor commits a buffer
        /// holding non-backdrop pixels (regression: Super opened a
        /// blank canvas — nothing reached the committed buffer).
        #[test]
        fn overview_open_paints_window_pixels_to_committed_buffer() {
            use crate::model::WindowEntry;
            use crate::overview::{BG, BYTES_PER_PIXEL};
            let mut comp = TestCompositor::new();
            comp.state.set_output_size(1280, 800);
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();
            let mut queue = conn.new_event_queue();
            let (mut host, _dir) = test_host();
            attach_host(&mut comp, &conn, &mut queue, &mut host);
            host.model.apply_window_list(
                vec![
                    WindowEntry::new(1, "alpha", true),
                    WindowEntry::new(2, "beta", false),
                ],
                vec![0],
            );
            host.model.set_overview_open(true);
            // Drive the production loop shape: pump server events,
            // then run the update pass (configure only stores the
            // size; painting happens on the next update pass).
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_overview(None);
            }

            fn backing_pixels(host: &mut ShellHost) -> Vec<u8> {
                use std::io::{Read, Seek, SeekFrom};
                let overview = host.overview.as_mut().expect("overview painted");
                let backing = overview.backing.as_mut().expect("overview committed");
                backing._file.seek(SeekFrom::Start(0)).expect("rewind");
                let mut out = Vec::new();
                backing._file.read_to_end(&mut out).expect("read");
                out
            }

            let pixels = backing_pixels(&mut host);
            let (chunks, _) = pixels.as_chunks::<BYTES_PER_PIXEL>();
            let painted = chunks.iter().filter(|pixel| **pixel != BG).count();
            assert!(
                painted > 0,
                "overview commits painted pixels, got {painted} non-backdrop of {}",
                chunks.len()
            );
        }

        /// Panel-only sessions paint the overview too: one iteration of
        /// the fallback loop shape (pump, shared update pass, flush)
        /// commits non-backdrop pixels. Regression: the fallback loop
        /// used to refresh panel status only, so Super opened a blank
        /// overview wherever no control socket was present.
        #[test]
        fn panel_only_iteration_paints_open_overview() {
            use crate::model::WindowEntry;
            use crate::overview::{BG, BYTES_PER_PIXEL};
            let mut comp = TestCompositor::new();
            comp.state.set_output_size(1280, 800);
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();
            let mut queue = conn.new_event_queue();
            let (mut host, _dir) = test_host();
            attach_host(&mut comp, &conn, &mut queue, &mut host);
            host.model
                .apply_window_list(vec![WindowEntry::new(1, "alpha", true)], vec![0]);
            host.model.set_overview_open(true);
            // First pass creates the surface and commits while the
            // configured size is still zero (nothing paints yet).
            host.update_overview(None);
            queue.flush().unwrap();
            comp.pump();
            // One fallback-loop iteration: dispatch the pending
            // configure, run the shared update pass, flush the paint.
            pump_server(&mut comp, &mut queue, &mut host);
            host.update_shell_surfaces(None);
            queue.flush().unwrap();

            let overview = host.overview.as_mut().expect("overview created");
            assert!(
                overview.width > 0 && overview.height > 0,
                "fallback iteration stores the configure"
            );
            let backing = overview.backing.as_mut().expect("overview committed");
            use std::io::{Read, Seek, SeekFrom};
            backing._file.seek(SeekFrom::Start(0)).expect("rewind");
            let mut pixels = Vec::new();
            backing._file.read_to_end(&mut pixels).expect("read");
            let (chunks, _) = pixels.as_chunks::<BYTES_PER_PIXEL>();
            let painted = chunks.iter().filter(|pixel| **pixel != BG).count();
            assert!(
                painted > 0,
                "fallback iteration commits painted pixels, got {painted} non-backdrop of {}",
                chunks.len()
            );
        }

        /// Output resize repaints at the new size: after the server
        /// re-arranges, the next update pass stores the new configure
        /// and commits a buffer of the new dimensions (no frozen or
        /// blank frame).
        #[test]
        fn overview_repaints_after_output_resize() {
            use crate::model::WindowEntry;
            let mut comp = TestCompositor::new();
            comp.state.set_output_size(1280, 800);
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();
            let mut queue = conn.new_event_queue();
            let (mut host, _dir) = test_host();
            attach_host(&mut comp, &conn, &mut queue, &mut host);
            host.model
                .apply_window_list(vec![WindowEntry::new(1, "alpha", true)], vec![0]);
            host.model.set_overview_open(true);
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_overview(None);
            }
            assert_eq!(
                host.overview.as_ref().map(|o| (o.width, o.height)),
                Some((1280, 800)),
                "overview sized at first geometry"
            );
            comp.state.set_output_size(1600, 900);
            roost_compositor::layer::arrange_after_commit(&comp.state);
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_overview(None);
                if host.overview.as_ref().is_some_and(|o| o.width == 1600) {
                    break;
                }
            }
            let overview = host.overview.as_ref().expect("overview created");
            assert_eq!(
                (overview.width, overview.height),
                (1600, 900),
                "overview tracks the resized output"
            );
            let len = overview.backing.as_ref().expect("repaint committed");
            use std::io::{Read, Seek, SeekFrom};
            let mut file = &len._file;
            file.seek(SeekFrom::Start(0)).expect("rewind");
            let mut pixels = Vec::new();
            file.read_to_end(&mut pixels).expect("read");
            assert_eq!(
                pixels.len(),
                1600 * 900 * crate::overview::BYTES_PER_PIXEL,
                "repainted buffer matches the new size"
            );
        }

        /// Empty overview still paints: with no windows open, the
        /// committed buffer exists at the configured size (backdrop,
        /// no crash, no missing commit).
        #[test]
        fn empty_overview_commits_backdrop() {
            let mut comp = TestCompositor::new();
            comp.state.set_output_size(1280, 800);
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();
            let mut queue = conn.new_event_queue();
            let (mut host, _dir) = test_host();
            attach_host(&mut comp, &conn, &mut queue, &mut host);
            host.model.set_overview_open(true);
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_overview(None);
            }
            let overview = host.overview.as_ref().expect("overview created");
            assert_eq!(
                (overview.width, overview.height),
                (1280, 800),
                "empty overview still sizes"
            );
            let backing = overview.backing.as_ref().expect("empty overview commits");
            use crate::overview::ACCENT;
            use std::io::{Read, Seek, SeekFrom};
            fn accent_count(backing: &ShmBacking) -> usize {
                use crate::overview::BYTES_PER_PIXEL;
                let mut file = &backing._file;
                file.seek(SeekFrom::Start(0)).expect("rewind");
                let mut pixels = Vec::new();
                file.read_to_end(&mut pixels).expect("read");
                assert_eq!(
                    pixels.len(),
                    1280 * 800 * crate::overview::BYTES_PER_PIXEL,
                    "empty overview commits a full-size buffer"
                );
                let (chunks, _) = pixels.as_chunks::<BYTES_PER_PIXEL>();
                chunks.iter().filter(|pixel| **pixel == ACCENT).count()
            }
            let before = accent_count(backing);
            // Two pinned favorites paint two 40x40 accent squares and
            // nothing else changes: the accent delta is exact.
            assert!(host.favorites.pin("alpha-app"));
            assert!(host.favorites.pin("beta-app"));
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_overview(None);
            }
            let after = accent_count(
                host.overview
                    .as_ref()
                    .expect("overview kept")
                    .backing
                    .as_ref()
                    .expect("repaint committed"),
            );
            assert_eq!(
                after - before,
                2 * 40 * 40,
                "two pinned favorites add two accent squares"
            );
        }

        /// Typing during paint loses nothing: five key presses driven
        /// through the real key path while update passes run arrive
        /// complete and in order.
        #[test]
        fn typing_during_paint_arrives_complete() {
            use super::super::{FocusRegion, ShellFocus};
            // evdev keycodes for h, e, l, l, o.
            const HELLO: [u32; 5] = [35, 18, 38, 38, 24];
            let mut comp = TestCompositor::new();
            comp.state.set_output_size(1280, 800);
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();
            let mut queue = conn.new_event_queue();
            let (mut host, _dir) = keyed_host();
            attach_host(&mut comp, &conn, &mut queue, &mut host);
            host.model.set_overview_open(true);
            host.focus = Some(ShellFocus {
                region: FocusRegion::Overview,
                cursor: 0,
            });
            for key in HELLO {
                host.on_key(key, true);
                host.on_key(key, false);
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_overview(None);
            }
            assert_eq!(
                host.search_text(),
                "hello",
                "all five keystrokes land during paint"
            );
        }

        /// Script cells reach the strip paint: a host with a cell
        /// script renders different pixels than one without.
        #[test]
        fn script_cell_paints_in_strip() {
            use crate::extensions::ExtensionHost;
            use std::fs;
            let plain_dir = tempfile::tempdir().expect("tempdir");
            let (mut plain, _dir) = test_host();
            plain.extensions = ExtensionHost::new(plain_dir.path().join("ext"));
            plain.extensions.load_dir();
            let script_dir = tempfile::tempdir().expect("tempdir");
            let ext = script_dir.path().join("ext");
            fs::create_dir_all(&ext).expect("mkdir");
            fs::write(ext.join("hello.rhai"), "bar_cell(\"hello\", \"\");").expect("write");
            let (mut host, _guard) = test_host();
            host.extensions = ExtensionHost::new(ext);
            host.extensions.load_dir();
            let a = plain.render_panel_pixels(1280, 32);
            let b = host.render_panel_pixels(1280, 32);
            assert_ne!(a, b, "script cell changes strip pixels");
        }

        /// Strip presses on a script cell dispatch its press action.
        #[test]
        fn strip_press_dispatches_script_action() {
            use crate::extensions::ExtensionHost;
            use crate::watcher::{indicator_cells, indicator_right_x};
            use std::fs;
            let script_dir = tempfile::tempdir().expect("tempdir");
            let ext = script_dir.path().join("ext");
            fs::create_dir_all(&ext).expect("mkdir");
            fs::write(
                ext.join("hello.rhai"),
                "bar_cell(\"hello\", \"\");\non_press(\"wave\");\nfn press(id) { notice(\"got \" + id); }",
            )
            .expect("write");
            let (mut host, _guard) = test_host();
            host.extensions = ExtensionHost::new(ext);
            host.extensions.load_dir();
            host.panel_size = Some((1280, 32));
            let strip_h = host.panel.height as i32;
            let rects = indicator_cells(indicator_right_x(1280), strip_h, 1);
            host.press_panel(rects[0].x + 1, rects[0].y + 1);
            assert_eq!(
                host.pending_presses.as_slice(),
                ["wave"],
                "strip press queues the action without running scripts"
            );
            host.drain_extension_presses();
            let output = host.extensions.output("hello").expect("hello loaded");
            assert!(
                output.notices.iter().any(|note| note.contains("got wave")),
                "strip press ran the press handler: {:?}",
                output.notices
            );
        }

        /// Script badges paint on the dock row past the last item.
        #[test]
        fn script_badge_paints_on_dock() {
            use crate::dock::DOCK_H;
            use crate::extensions::ExtensionHost;
            use std::fs;
            let plain_dir = tempfile::tempdir().expect("tempdir");
            let (mut plain, _dir) = test_host();
            plain.extensions = ExtensionHost::new(plain_dir.path().join("ext"));
            plain.extensions.load_dir();
            let script_dir = tempfile::tempdir().expect("tempdir");
            let ext = script_dir.path().join("ext");
            fs::create_dir_all(&ext).expect("mkdir");
            fs::write(ext.join("stat.rhai"), "dock_badge(\"42\");").expect("write");
            let (mut host, _guard) = test_host();
            host.extensions = ExtensionHost::new(ext);
            host.extensions.load_dir();
            let items: Vec<crate::dock::DockItem> = Vec::new();
            let a = plain.render_dock_pixels(1280, DOCK_H, &items);
            let b = host.render_dock_pixels(1280, DOCK_H, &items);
            assert_ne!(a, b, "script badge changes dock pixels");
        }

        /// Script notices surface as notifications exactly once each.
        #[test]
        fn script_notice_surfaces_once() {
            use crate::extensions::ExtensionHost;
            use std::fs;
            let script_dir = tempfile::tempdir().expect("tempdir");
            let ext = script_dir.path().join("ext");
            fs::create_dir_all(&ext).expect("mkdir");
            fs::write(ext.join("hello.rhai"), "notice(\"hi there\");").expect("write");
            let (mut host, _guard) = test_host();
            host.extensions = ExtensionHost::new(ext);
            host.extensions.load_dir();
            host.surface_extension_notes();
            let unread = host
                .center
                .lock()
                .map(|center| center.unread_count())
                .unwrap_or(0);
            assert_eq!(unread, 1, "notice banners once");
            host.surface_extension_notes();
            let again = host
                .center
                .lock()
                .map(|center| center.unread_count())
                .unwrap_or(0);
            assert_eq!(again, 1, "no duplicate banner on re-drive");
        }

        /// Editing a script applies on the next drive; removing it
        /// drops its output without restart.
        #[test]
        fn edited_script_reloads_and_removed_drops() {
            use crate::extensions::ExtensionHost;
            use std::fs;
            let script_dir = tempfile::tempdir().expect("tempdir");
            let ext = script_dir.path().join("ext");
            fs::create_dir_all(&ext).expect("mkdir");
            fs::write(ext.join("stat.rhai"), "bar_cell(\"one\", \"\");").expect("write");
            let (mut host, _guard) = test_host();
            host.extensions = ExtensionHost::new(ext.clone());
            host.extensions.load_dir();
            assert_eq!(
                host.extensions
                    .output("stat")
                    .expect("stat loaded")
                    .cell_text
                    .as_deref(),
                Some("one")
            );
            fs::write(ext.join("stat.rhai"), "bar_cell(\"two\", \"\");").expect("rewrite");
            host.extensions.drive();
            assert_eq!(
                host.extensions
                    .output("stat")
                    .expect("stat kept")
                    .cell_text
                    .as_deref(),
                Some("two"),
                "edit applies without restart"
            );
            fs::remove_file(ext.join("stat.rhai")).expect("remove");
            host.extensions.drive();
            assert!(
                host.extensions.output("stat").is_none(),
                "removal drops the script"
            );
        }

        /// Registry observer on a throwaway queue, used only to learn
        /// global names before binding on the panel queue.
        #[derive(Default)]
        struct Collector {
            compositor: Option<(u32, u32)>,
            layer_shell: Option<(u32, u32)>,
            shm: Option<(u32, u32)>,
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
                        "wl_shm" => state.shm = Some((name, version)),
                        _ => {}
                    }
                }
            }
        }

        /// Bind compositor, layer-shell, and shm on the panel queue and
        /// attach the host (banner/switcher surfaces need all three).
        fn attach_host(
            comp: &mut TestCompositor,
            conn: &Connection,
            queue: &mut EventQueue<ShellHost>,
            host: &mut ShellHost,
        ) {
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
                if collector.compositor.is_some()
                    && collector.layer_shell.is_some()
                    && collector.shm.is_some()
                {
                    break;
                }
            }
            let (compositor_name, compositor_version) =
                collector.compositor.expect("wl_compositor advertised");
            let (layer_name, layer_version) = collector
                .layer_shell
                .expect("zwlr_layer_shell_v1 advertised");
            let (shm_name, shm_version) = collector.shm.expect("wl_shm advertised");
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
            let shm: WlShm =
                aux_registry.bind::<WlShm, _, _>(shm_name, shm_version.min(1), &qh, ());
            drop((aux, aux_registry, collector));
            host.attach_wayland(compositor, layer_shell, shm, qh);
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

        /// Banner surfaces follow the notification queue: notify opens
        /// a bottom-right overlay strip under our namespace, draining
        /// the queue destroys it again (explicit destroy, like the
        /// overview and switcher).
        #[test]
        fn banners_surface_appears_and_destroys_with_queue() {
            use crate::notifications::Urgency;

            let mut comp = TestCompositor::new();
            comp.state.set_output_size(1280, 800);
            let (server_stream, client_stream) = UnixStream::pair().unwrap();
            comp.add_client(server_stream);
            let conn = Connection::from_socket(client_stream).unwrap();
            let mut queue = conn.new_event_queue();
            let (mut host, _dir) = test_host();
            attach_host(&mut comp, &conn, &mut queue, &mut host);

            let first = host
                .notification_center()
                .lock()
                .expect("center lock")
                .notify(
                    "app",
                    "t1",
                    "b1",
                    vec![],
                    crate::notifications::Urgency::Normal,
                    None,
                );
            let crit = host
                .notification_center()
                .lock()
                .expect("center lock")
                .notify("app", "t2", "b2", vec![], Urgency::Critical, None);
            host.update_banners();
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_banners();
                if comp
                    .state
                    .panel_surfaces()
                    .iter()
                    .any(|s| s.namespace == BANNER_NAMESPACE && s.configured)
                {
                    break;
                }
            }
            let banners: Vec<_> = comp
                .state
                .panel_surfaces()
                .into_iter()
                .filter(|s| s.namespace == BANNER_NAMESPACE)
                .collect();
            assert_eq!(banners.len(), 1, "one banner strip tracked");
            assert!(banners[0].configured, "banner acked the configure");
            assert_eq!(
                banners[0].layer,
                smithay::wayland::shell::wlr_layer::Layer::Overlay
            );

            // Draining the queue destroys the surface server-side too.
            host.notification_center()
                .lock()
                .expect("center lock")
                .dismiss(first)
                .unwrap();
            host.notification_center()
                .lock()
                .expect("center lock")
                .dismiss(crit)
                .unwrap();
            host.update_banners();
            for _ in 0..PUMP_ROUNDS {
                pump_server(&mut comp, &mut queue, &mut host);
                host.update_banners();
                if comp
                    .state
                    .panel_surfaces()
                    .iter()
                    .all(|s| s.namespace != BANNER_NAMESPACE)
                {
                    break;
                }
            }
            assert!(
                comp.state
                    .panel_surfaces()
                    .iter()
                    .all(|s| s.namespace != BANNER_NAMESPACE),
                "banner strip destroyed with the queue"
            );
        }

        /// Banner presses route without Wayland: the dismiss box
        /// closes, the row body invokes `default`. The consumed
        /// action key proves the body press invoked rather than
        /// dismissed (dismiss never consumes keys); history survives
        /// both either way.
        #[test]
        fn banner_presses_invoke_default_or_dismiss() {
            use crate::notifications::NotificationAction;
            use crate::overview::{banner_row_boxes, BANNER_STRIP_W};

            let (mut host, _dir) = test_host();
            let center = host.notification_center();
            let action = |id: &str| NotificationAction {
                id: id.to_owned(),
                label: format!("label-{id}"),
            };
            let invoked = center.lock().expect("center lock").notify(
                "app",
                "Hello",
                "a stub body",
                vec![action("default")],
                crate::notifications::Urgency::Normal,
                None,
            );
            center.lock().expect("center lock").notify(
                "app",
                "Plain",
                "",
                vec![],
                crate::notifications::Urgency::Normal,
                None,
            );

            // Row body on the `default` banner invokes the key.
            let boxes = banner_row_boxes(&[false, false], BANNER_STRIP_W, 0).expect("row 0");
            host.press_banner(boxes.row.0 + 10, boxes.row.1 + 40);
            {
                let guard = center.lock().expect("center lock");
                let live: Vec<u64> = guard.banners().iter().map(|n| n.id).collect();
                assert!(!live.contains(&invoked), "invoke clears the banner");
                assert_eq!(live.len(), 1, "only the pressed banner leaves");
                let entry = guard
                    .history()
                    .iter()
                    .find(|n| n.id == invoked)
                    .expect("history");
                assert!(
                    entry.pending_actions().is_empty(),
                    "body press invoked default (dismiss consumes nothing)"
                );
                assert_eq!(guard.history().len(), 2, "invoke keeps history");
            }

            // The plain banner slid into row 0; its dismiss box closes
            // it, and the body of a banner with no default key falls
            // back to dismiss.
            let boxes = banner_row_boxes(&[false], BANNER_STRIP_W, 0).expect("row 0");
            host.press_banner(boxes.dismiss.0 + 2, boxes.dismiss.1 + 2);
            assert!(
                center.lock().expect("center lock").banners().is_empty(),
                "dismiss press closes"
            );
        }

        /// Banner presses never take keyboard focus and never disturb
        /// window selection: action, dismiss, and miss presses leave
        /// the selected window, the closed overview, and the empty
        /// search box exactly alone. (The layer itself is created with
        /// `KeyboardInteractivity::None`; this pins the press path.)
        #[test]
        fn banner_presses_never_touch_selection_or_focus() {
            use crate::model::WindowEntry;
            use crate::notifications::NotificationAction;
            use crate::overview::{banner_row_boxes, BANNER_STRIP_W};

            let (mut host, _dir) = test_host();
            host.model.apply_window_list(
                vec![
                    WindowEntry::new(1, "a", true),
                    WindowEntry::new(2, "b", false),
                ],
                vec![0],
            );
            assert_eq!(host.model.selected(), Some(1));
            let center = host.notification_center();
            center.lock().expect("center lock").notify(
                "app",
                "Hello",
                "a stub body",
                vec![NotificationAction {
                    id: "default".to_owned(),
                    label: "Open".to_owned(),
                }],
                crate::notifications::Urgency::Critical,
                None,
            );

            // Action-row press invokes.
            let boxes = banner_row_boxes(&[false], BANNER_STRIP_W, 0).expect("row 0");
            host.press_banner(boxes.row.0 + 10, boxes.row.1 + 40);
            // Re-file and dismiss-press.
            center.lock().expect("center lock").notify(
                "app",
                "Again",
                "",
                vec![],
                crate::notifications::Urgency::Normal,
                None,
            );
            let boxes = banner_row_boxes(&[false], BANNER_STRIP_W, 0).expect("row 0");
            host.press_banner(boxes.dismiss.0 + 2, boxes.dismiss.1 + 2);
            // Miss press on the drained queue.
            host.press_banner(4, 4);

            assert_eq!(host.model.selected(), Some(1), "selection stable");
            assert!(!host.model.is_overview_open(), "overview stays shut");
            assert!(host.search_text().is_empty(), "no key text typed");
            assert!(!host.take_submit() && !host.take_dismiss(), "no key arms");
        }

        /// Bar presence marker: the panel paint key carries the
        /// center's unread count, so one slow tick picks up queue
        /// changes; zero unread leaves the marker out.
        #[test]
        fn bar_marker_key_tracks_unread_within_one_tick() {
            use crate::notifications::Urgency;

            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            // Pin the slow-tick throttle: probes stay fixed and each
            // tick below only exercises the repaint decision.
            let tick = |host: &mut ShellHost| {
                host.tiles_refreshed = Some(std::time::Instant::now());
                host.update_panel_status();
            };
            tick(&mut host);
            let quiet = host.panel_paint_key.clone().expect("tick records key");
            assert_eq!(quiet.unread, 0, "no marker at zero");

            let id = host
                .notification_center()
                .lock()
                .expect("center lock")
                .notify("app", "t", "b", vec![], Urgency::Normal, None);
            tick(&mut host);
            let marked = host.panel_paint_key.clone().expect("tick records key");
            assert_eq!(marked.unread, 1, "marker visible with unread queued");
            assert_ne!(quiet, marked, "queue change repaints within one tick");

            host.notification_center()
                .lock()
                .expect("center lock")
                .dismiss(id)
                .unwrap();
            tick(&mut host);
            let cleared = host.panel_paint_key.clone().expect("tick records key");
            assert_eq!(cleared.unread, 0, "marker hidden when none");
            assert_eq!(quiet, cleared, "drain restores the quiet key");
        }

        /// Flood through the host: ten thousand notifies keep the
        /// banner paint rows bounded, newest-kept, and stable — the
        /// same key twice with no mutation between, and the panel
        /// marker tracks the capped unread count within one tick.
        #[test]
        fn flood_keeps_banner_rows_bounded_and_stable() {
            use super::super::banner_key_rows;
            use crate::notifications::{Urgency, MAX_BANNERS, MAX_HISTORY};

            let (mut host, _dir) = test_host();
            host.panel_size = Some((1280, 32));
            host.tiles.clock = "12:34".to_owned();
            let center = host.notification_center();
            for _ in 0..10_000 {
                center.lock().expect("center lock").notify(
                    "app",
                    "flood",
                    "body",
                    vec![],
                    Urgency::Normal,
                    None,
                );
            }
            {
                let guard = center.lock().expect("center lock");
                assert_eq!(guard.history().len(), MAX_HISTORY, "history capped");
                assert_eq!(guard.unread_count(), MAX_BANNERS, "unread capped");
                let rows = banner_key_rows(&guard);
                assert_eq!(
                    rows.len(),
                    MAX_BANNERS,
                    "paint rows never grow past the cap"
                );
                let newest: Vec<u64> = guard.banners().iter().map(|n| n.id).collect();
                assert_eq!(
                    rows.iter().map(|row| row.id).collect::<Vec<_>>(),
                    newest,
                    "paint rows track the newest banners"
                );
                // No mutation between builds: the key is stable.
                assert_eq!(rows, banner_key_rows(&guard), "paint rows stable at rest");
            }
            // One slow tick picks up the capped marker (not 10k).
            host.tiles_refreshed = Some(std::time::Instant::now());
            host.update_panel_status();
            let key = host.panel_paint_key.clone().expect("tick records key");
            assert_eq!(key.unread, MAX_BANNERS, "marker shows the capped queue");
        }

        /// Restart rehydrates the unread list: save the queue, rebuild
        /// the host from it (the same `restore_notification_queue`
        /// seam `run_panel_with_control` drives via
        /// `load_notification_queue`), and the banners shown before
        /// are the banners shown after.
        #[test]
        fn restart_rehydrates_the_unread_list() {
            use crate::notifications::Urgency;

            let dir = tempfile::tempdir().expect("tempdir");
            let queue = dir.path().join("notifications.json");
            let (host, _guard) = test_host();
            let center = host.notification_center();
            let first = center.lock().expect("center lock").notify(
                "app",
                "first",
                "b1",
                vec![],
                Urgency::Normal,
                None,
            );
            let second = center.lock().expect("center lock").notify(
                "app",
                "second",
                "b2",
                vec![],
                Urgency::Critical,
                None,
            );
            center
                .lock()
                .expect("center lock")
                .save(&queue)
                .expect("save");
            let before: Vec<(u64, String, String)> = center
                .lock()
                .expect("center lock")
                .banners()
                .iter()
                .map(|n| (n.id, n.summary().to_owned(), n.body().to_owned()))
                .collect();
            assert_eq!(before.len(), 2);
            drop(center);
            drop(host);

            // Rebuilt host over the saved file.
            let (mut restarted, _guard) = test_host();
            restarted.restore_notification_queue(&queue);
            let after: Vec<(u64, String, String)> = restarted
                .notification_center()
                .lock()
                .expect("center lock")
                .banners()
                .iter()
                .map(|n| (n.id, n.summary().to_owned(), n.body().to_owned()))
                .collect();
            assert_eq!(after, before, "restart preserves the unread list");
            assert_eq!((first, second), (before[0].0, before[1].0));
            // The rehydrated center keeps filing onto the same file.
            assert_eq!(
                restarted
                    .notification_center()
                    .lock()
                    .expect("center lock")
                    .queue_path(),
                Some(queue.as_path())
            );
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

        /// Overview intent flips park and restore the shell cursor:
        /// a fresh open pushes the live stop and takes result
        /// focus, and the close settles back to the parked stop —
        /// the same `sync_overview` path the run loop drives.
        #[test]
        fn overview_intent_parks_and_restores_focus() {
            use super::super::{FocusRegion, ShellFocus};
            use std::rc::Rc;

            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("control.sock");
            let mut model = StateModel::new();
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
            assert!(!host.model.is_overview_open());
            host.focus = Some(ShellFocus {
                region: FocusRegion::Panel,
                cursor: 2,
            });
            hub.set_overview(true);
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                let _ = client.poll();
                if client.model().is_overview_open() {
                    break;
                }
            }
            host.sync_overview(&client);
            assert!(host.model.is_overview_open());
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Overview,
                    cursor: 0,
                }),
                "open takes result focus"
            );
            assert_eq!(
                host.return_stack,
                vec![ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 2,
                }],
                "open parks the live stop"
            );

            hub.set_overview(false);
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                let _ = client.poll();
                if !client.model().is_overview_open() {
                    break;
                }
            }
            host.sync_overview(&client);
            assert!(!host.model.is_overview_open());
            assert_eq!(
                host.shell_focus(),
                Some(ShellFocus {
                    region: FocusRegion::Panel,
                    cursor: 2,
                }),
                "close restores the parked stop"
            );
            assert!(host.return_stack.is_empty());
        }

        /// Alt-Tab drive round trip: hub-queued steps open the host
        /// switcher on the MRU previous window, commit activates
        /// through the token gate, cancel closes without touching
        /// focus. Same `sync_overview` path the run loop drives.
        #[test]
        fn switcher_drive_steps_and_commits() {
            use roost_shell_control::{CommandStatus, SwitcherAction};

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

        /// Overview keys type, erase, submit, and dismiss — but only
        /// while open. Closed-overview keys must not touch the buffer
        /// or arm activations (focus is elsewhere then).
        #[test]
        fn overview_keys_feed_search_only_while_open() {
            use crate::keyboard::XkbFeed;

            // Evdev codes on a `us` layout.
            const A: u32 = 30;
            const B: u32 = 48;
            const SPACE: u32 = 57;
            const SHIFT: u32 = 42;
            const BACKSPACE: u32 = 14;
            const RETURN: u32 = 28;
            const ESCAPE: u32 = 1;

            let (mut host, _favdir) = test_host();
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);

            host.on_key(A, true);
            host.on_key(RETURN, true);
            assert_eq!(host.search_text(), "", "closed overview types nothing");
            assert!(!host.take_submit(), "closed overview submits nothing");
            assert!(!host.take_dismiss(), "closed overview dismisses nothing");

            host.model.set_overview_open(true);
            host.on_key(A, true);
            host.on_key(A, false);
            host.on_key(SPACE, true);
            host.on_key(SPACE, false);
            host.on_key(B, true);
            host.on_key(B, false);
            assert_eq!(host.search_text(), "a b");
            host.on_key(BACKSPACE, true);
            assert_eq!(host.search_text(), "a ");
            host.on_key(RETURN, true);
            assert!(host.take_submit(), "enter arms submit");
            assert!(!host.take_submit(), "submit is single-shot");
            host.on_key(ESCAPE, true);
            assert!(host.take_dismiss(), "escape arms dismiss");

            host.on_key(SHIFT, true);
            host.on_key(A, true);
            host.on_key(A, false);
            host.on_key(SHIFT, false);
            assert!(
                host.search_text().ends_with('A'),
                "shift applies through the feed, got {:?}",
                host.search_text()
            );
        }

        /// `on_key` queues navigation into `take_nav` in both overview
        /// states (Task 1 routing): the strip is visible with the
        /// overview closed, and the focus model drains the queue.
        /// Presses only; search text and submit/dismiss stay untouched.
        #[test]
        fn on_key_queues_nav_in_both_overview_states() {
            use crate::keyboard::{KeyAction, XkbFeed};

            // Evdev codes on a `us` layout.
            const UP: u32 = 103;
            const DOWN: u32 = 108;
            const LEFT: u32 = 105;
            const RIGHT: u32 = 106;
            const TAB: u32 = 15;
            const SHIFT: u32 = 42;
            const F6: u32 = 64;
            const D: u32 = 32;
            const SUPER: u32 = 125;
            const F1: u32 = 59;
            const ALT: u32 = 56;

            let press_nav = |host: &mut ShellHost| {
                host.on_key(UP, true);
                host.on_key(DOWN, true);
                host.on_key(LEFT, true);
                host.on_key(RIGHT, true);
                host.on_key(TAB, true);
                host.on_key(SHIFT, true);
                host.on_key(TAB, true);
                host.on_key(SHIFT, false);
                host.on_key(F6, true);
                host.on_key(SUPER, true);
                host.on_key(D, true);
                host.on_key(SUPER, false);
                host.on_key(ALT, true);
                host.on_key(F1, true);
                host.on_key(ALT, false);
            };

            let want = vec![
                KeyAction::Up,
                KeyAction::Down,
                KeyAction::Left,
                KeyAction::Right,
                KeyAction::Next,
                KeyAction::Previous,
                KeyAction::CycleRegion,
                KeyAction::FocusDock,
                KeyAction::FocusPanel,
            ];

            for open in [false, true] {
                let (mut host, _favdir) = test_host();
                host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);
                host.model.set_overview_open(open);
                press_nav(&mut host);
                assert_eq!(host.take_nav(), want, "nav queues while open={open}");
                assert!(host.take_nav().is_empty(), "take_nav drains");
                assert_eq!(host.search_text(), "", "nav never types");
                assert!(!host.take_submit(), "nav never submits");
                assert!(!host.take_dismiss(), "nav never dismisses");
            }
        }

        /// Releases never queue navigation, and search gating beside
        /// the nav path is unchanged: closed-overview text keys stay
        /// out of the buffer while nav still queues.
        #[test]
        fn nav_releases_queue_nothing_and_closed_search_stays_gated() {
            use crate::keyboard::XkbFeed;

            const A: u32 = 30;
            const RETURN: u32 = 28;
            const BACKSPACE: u32 = 14;
            const UP: u32 = 103;
            const TAB: u32 = 15;
            const F6: u32 = 64;

            let (mut host, _favdir) = test_host();
            host.xkb = XkbFeed::from_names("evdev", "pc105", "us", "", None);

            // Closed overview: text/erase/submit stay gated out, but
            // navigation still queues.
            host.on_key(A, true);
            host.on_key(BACKSPACE, true);
            host.on_key(RETURN, true);
            assert_eq!(host.search_text(), "", "closed overview types nothing");
            assert!(!host.take_submit(), "closed overview submits nothing");
            host.on_key(UP, true);
            host.on_key(TAB, true);
            host.on_key(F6, true);
            assert_eq!(host.take_nav().len(), 3, "nav queues while closed");
            // Releases queue nothing.
            host.on_key(UP, false);
            host.on_key(TAB, false);
            host.on_key(F6, false);
            host.on_key(A, false);
            assert!(host.take_nav().is_empty(), "releases queue nothing");
        }

        /// Enter on a window hit focuses through the token gate and
        /// dismisses the overview; the compositor confirms both.
        #[test]
        fn activate_window_hit_focuses_and_dismisses() {
            use crate::search::SearchResult;
            use roost_shell_control::CommandStatus;

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

        /// Manual lock from the shell trigger engages the lock screen
        /// at once: the trigger sends `Lock`, the hub applies it, and
        /// the locked, content-free snapshot lands on the display
        /// value with no window content.
        #[test]
        fn manual_lock_engages_lock_screen_at_once() {
            use roost_shell_control::CommandStatus;

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
            let (host, _favdir) = test_host();
            assert!(!client.locked(), "unlocked snapshot first");
            assert_eq!(client.model().windows().len(), 2);

            let request = host.lock_now(&mut client).expect("lock send");

            let mut result = None;
            let mut locked_seen = false;
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::CommandResult { id, status }) => {
                        if id == request {
                            result = Some(status);
                        }
                    }
                    Ok(Handled::Snapshot { .. }) => {
                        if client.locked() {
                            locked_seen = true;
                        }
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("lock poll failed: {e}"),
                }
                if result.is_some() && locked_seen {
                    break;
                }
            }
            assert_eq!(result, Some(CommandStatus::Applied));
            assert!(locked_seen, "locked snapshot lands at once");
            assert!(client.locked(), "display reads the snapshot flag");
            assert!(
                client.model().windows().is_empty(),
                "no window content while locked"
            );
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

        /// Type-and-press on a fixture app spawns it with starting
        /// feedback and dismisses the overview: the behavioural proof
        /// for the press-launch path.
        #[test]
        fn overview_press_launches_typed_app_with_feedback() {
            use crate::apps::discover;
            use std::rc::Rc;

            let apps_dir = tempfile::tempdir().unwrap();
            std::fs::write(
                apps_dir.path().join("sleeper.desktop"),
                "[Desktop Entry]\nName=Sleeper Probe\nExec=/bin/sleep 30\nType=Application\n",
            )
            .unwrap();
            let fav_dir = tempfile::tempdir().unwrap();
            let mut host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(discover(&[apps_dir.path().to_owned()])),
                Favorites::load(fav_dir.path().join(crate::favorites::FAVORITES_FILE)),
            );

            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("control.sock");
            let mut model = StateModel::new();
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

            // Type the fixture name; the hub answers off-thread.
            host.search_query("sleeper probe");
            let mut results = Vec::new();
            for _ in 0..200 {
                results = host.search_collect();
                if results.iter().any(|hit| hit.title == "Sleeper Probe") {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let index = results
                .iter()
                .position(|hit| hit.title == "Sleeper Probe")
                .expect("typed query answers the fixture app");
            let app_id = match &results[index].action {
                crate::search::SearchAction::Launch { app_id } => app_id.clone(),
                other => panic!("fixture hit must launch, got {other:?}"),
            };

            // Press the app result: spawn plus starting feedback.
            match host.press_overview(&mut client, &results, index, crate::dock::BTN_LEFT) {
                Ok(Some(super::super::HitOutcome::Launched { pid })) => {
                    assert!(pid > 0, "spawn reports a live pid");
                }
                other => panic!("expected launch, got {other:?}"),
            }
            assert!(
                host.launch_states()
                    .get(&app_id)
                    .is_some_and(|states| states.contains(&crate::apps::LaunchState::Starting)),
                "long-lived spawn shows starting feedback"
            );

            // The hub applies the dismissal flip after the spawn.
            let mut dismissed = false;
            for _ in 0..PUMP_ROUNDS {
                hub.poll(&mut model);
                match client.poll() {
                    Ok(Handled::Overview { open }) => {
                        if !open {
                            dismissed = true;
                            break;
                        }
                    }
                    Ok(_) => continue,
                    Err(e) if is_would_block(&e) => continue,
                    Err(e) => panic!("dismiss poll failed: {e}"),
                }
            }
            assert!(dismissed, "overview dismisses after press-launch");
        }

        /// Press on an unlaunchable entry fails closed: spawn error,
        /// overview still open, tracker empty. Inline rendering
        /// belongs to the failure task; this pins the no-panic,
        /// no-dismiss floor it builds on.
        #[test]
        fn overview_press_spawn_failure_keeps_overview_open() {
            use crate::apps::discover;
            use crate::search::SearchProvider;

            let apps_dir = tempfile::tempdir().unwrap();
            std::fs::write(
                apps_dir.path().join("broken.desktop"),
                "[Desktop Entry]\nName=Broken Probe\nExec=/nonexistent-roost-binary-xyz\nType=Application\n",
            )
            .unwrap();
            let fav_dir = tempfile::tempdir().unwrap();
            let mut host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(discover(&[apps_dir.path().to_owned()])),
                Favorites::load(fav_dir.path().join(crate::favorites::FAVORITES_FILE)),
            );
            host.model.set_overview_open(true);
            let (stream, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            let mut client = ControlClient::new(stream).expect("client");

            let provider = AppProvider::new(discover(&[apps_dir.path().to_owned()]));
            let results = provider.query("broken");
            assert_eq!(results.len(), 1, "fixture app answers the query");
            match host.press_overview(&mut client, &results, 0, crate::dock::BTN_LEFT) {
                Err(super::super::HitError::Launch(_)) => {}
                other => panic!("expected launch failure, got {other:?}"),
            }
            assert!(
                host.model.is_overview_open(),
                "failed press keeps the overview open"
            );
            assert!(
                host.launcher.states().is_empty(),
                "failed spawn tracks nothing"
            );
        }

        /// Bad-Exec press reports inline: the spawn error keeps the
        /// overview open with failure state naming the pressed row,
        /// and the repaint marker lands a brick border on that row.
        #[test]
        fn overview_press_spawn_failure_reports_inline() {
            use crate::apps::discover;
            use crate::overview::{overview_result_box, OverviewCanvas, WARN};
            use crate::search::SearchProvider;

            let apps_dir = tempfile::tempdir().unwrap();
            std::fs::write(
                apps_dir.path().join("broken.desktop"),
                "[Desktop Entry]\nName=Broken Probe\nExec=/nonexistent-roost-binary-xyz\nType=Application\n",
            )
            .unwrap();
            let fav_dir = tempfile::tempdir().unwrap();
            let mut host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(discover(&[apps_dir.path().to_owned()])),
                Favorites::load(fav_dir.path().join(crate::favorites::FAVORITES_FILE)),
            );
            host.model.set_overview_open(true);
            let (stream, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            let mut client = ControlClient::new(stream).expect("client");

            let provider = AppProvider::new(discover(&[apps_dir.path().to_owned()]));
            let results = provider.query("broken");
            assert_eq!(results.len(), 1, "fixture app answers the query");
            match host.press_overview(&mut client, &results, 0, crate::dock::BTN_LEFT) {
                Err(super::super::HitError::Launch(_)) => {}
                other => panic!("expected launch failure, got {other:?}"),
            }
            assert!(
                host.model.is_overview_open(),
                "failed press keeps the overview open"
            );
            assert!(
                host.launcher.states().is_empty(),
                "failed spawn tracks nothing"
            );
            // Inline failure names the pressed row with the error.
            let failure = host
                .overview_failure()
                .expect("failed press records inline failure");
            assert_eq!(failure.index, 0, "marker lands on the pressed row");
            assert!(
                failure.message.contains("launch failed"),
                "message names the failure: {}",
                failure.message
            );
            // The repaint marker borders exactly that row's box.
            let (x, y, w, h) = overview_result_box(failure.index).expect("failed row box");
            let mut canvas = OverviewCanvas::new(1280, 800);
            canvas.render(&host.model, 0);
            canvas.draw_launch_failure(failure.index);
            let at = |x: i32, y: i32| {
                let at = (y as usize * 1280 + x as usize) * crate::overview::BYTES_PER_PIXEL;
                <[u8; 4]>::try_from(&canvas.pixels()[at..at + 4]).unwrap()
            };
            assert_eq!(at(x, y), WARN, "inline marker borders the pressed row");
            assert_eq!(at(x + w - 1, y + h - 1), WARN);
            assert_eq!(at(x + 8, y + 8), WARN, "filled tick marks the row");
        }

        /// Press on a window (action) result runs its activation
        /// command through the token gate and dismisses the
        /// overview: the behavioural proof for the action-run path.
        #[test]
        fn overview_press_action_runs_command_and_dismisses() {
            use crate::search::SearchResult;
            use roost_shell_control::CommandStatus;
            use std::rc::Rc;

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

            // Press the window result: the activation command runs.
            let results = vec![SearchResult::focus("beta", None, id_b)];
            let request = match host.press_overview(&mut client, &results, 0, crate::dock::BTN_LEFT)
            {
                Ok(Some(super::super::HitOutcome::Focused { request })) => request,
                other => panic!("expected focus, got {other:?}"),
            };
            assert_eq!(host.model.selected(), Some(id_b));

            // The hub applies the activation, then the dismissal flip.
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
            assert!(dismissed, "overview dismisses after action press");
            assert_eq!(model.focused(), Some(id_b));
        }

        /// Presses that route nowhere are no-ops: no spawn, no
        /// dismissal, overview untouched.
        #[test]
        fn overview_press_miss_is_noop() {
            use crate::search::SearchResult;

            let (mut host, _dir) = test_host();
            host.model.set_overview_open(true);
            let (stream, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
            let mut client = ControlClient::new(stream).expect("client");

            let results = vec![SearchResult::launch("True", "true")];
            // Wrong button and stale index route nowhere.
            assert!(host
                .press_overview(&mut client, &results, 0, crate::dock::BTN_RIGHT)
                .expect("miss must not fail")
                .is_none());
            assert!(host
                .press_overview(&mut client, &results, 9, crate::dock::BTN_LEFT)
                .expect("miss must not fail")
                .is_none());
            assert!(host.model.is_overview_open());
            assert!(host.launcher.states().is_empty());
        }

        /// Env mutation is process-global while Rust runs tests in
        /// parallel threads, so every env-touching test below holds
        /// this lock (same pattern as the icons.rs fixture tests).
        static ENV_LOCK: Mutex<()> = Mutex::new(());

        struct EnvRestore {
            data_home: Option<std::ffi::OsString>,
            data_dirs: Option<std::ffi::OsString>,
            home: Option<std::ffi::OsString>,
        }

        impl EnvRestore {
            /// Point the icon resolver at `tmp`, away from real
            /// system dirs.
            fn install(tmp: &tempfile::TempDir) -> EnvRestore {
                let prev = EnvRestore {
                    data_home: std::env::var_os("XDG_DATA_HOME"),
                    data_dirs: std::env::var_os("XDG_DATA_DIRS"),
                    home: std::env::var_os("HOME"),
                };
                std::env::set_var("XDG_DATA_HOME", tmp.path());
                std::env::set_var("XDG_DATA_DIRS", tmp.path().join("empty-dirs"));
                std::env::set_var("HOME", tmp.path().join("home"));
                prev
            }

            fn restore(key: &str, value: &Option<std::ffi::OsString>) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }

        impl Drop for EnvRestore {
            fn drop(&mut self) {
                EnvRestore::restore("XDG_DATA_HOME", &self.data_home);
                EnvRestore::restore("XDG_DATA_DIRS", &self.data_dirs);
                EnvRestore::restore("HOME", &self.home);
            }
        }

        /// Theme tree holding one PNG icon: `<tmp>/icons/<theme>`
        /// with a `32x32/apps` raster. Caller holds `ENV_LOCK` and
        /// installs `EnvRestore` over the result.
        fn icon_theme_fixture(theme: &str, name: &str) -> tempfile::TempDir {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp
                .path()
                .join("icons")
                .join(theme)
                .join("32x32/apps")
                .join(format!("{name}.png"));
            std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("mkdirs");
            let mut img = image::RgbaImage::new(32, 32);
            for px in img.pixels_mut() {
                *px = image::Rgba([0, 128, 255, 255]);
            }
            img.save(&path).expect("save png");
            tmp
        }

        fn dummy_artwork() -> crate::icons::Artwork {
            crate::icons::Artwork {
                size: 32,
                argb: vec![0; 32 * 32 * 4],
            }
        }

        fn some_panel_key() -> super::super::PanelPaintKey {
            super::super::PanelPaintKey {
                clock: "12:34".to_owned(),
                clock_format: crate::settings::ClockFormat::TwentyFour,
                states: [(crate::tiles::TileState::Ready, None); 3],
                unread: 0,
                width: 1280,
                height: 32,
                focus: None,
                popup: None,
                indicators: Vec::new(),
                extensions: Vec::new(),
            }
        }

        fn some_dock_key() -> super::super::DockPaintKey {
            super::super::DockPaintKey {
                items: vec![("org.example.True.desktop".to_owned(), vec![], false, true)],
                badges: Vec::new(),
                width: 1280,
                height: crate::dock::DOCK_H,
                stack: None,
                focus: None,
            }
        }

        /// A snapshot theme flip clears the artwork cache and bumps
        /// both paint keys so bars and docks repaint with the new
        /// artwork on the same tick.
        #[test]
        fn sync_icon_theme_clears_cache_and_bumps_paint_keys_on_flip() {
            let (mut host, _dir) = test_host();
            host.icon_cache.insert(
                (host.icon_theme.clone(), "roost-cached".to_owned(), 32),
                dummy_artwork(),
            );
            host.panel_paint_key = Some(some_panel_key());
            host.dock_paint_key = Some(some_dock_key());
            host.tiles.settings.icon_theme = "HighContrast".to_owned();

            host.sync_icon_theme();

            assert!(host.icon_cache.is_empty(), "theme flip drops cached art");
            assert_eq!(host.panel_paint_key, None, "panel repaints after flip");
            assert_eq!(host.dock_paint_key, None, "dock repaints after flip");
            assert_eq!(host.icon_theme, "HighContrast");
        }

        /// Same theme on the snapshot is a no-op: cache and paint
        /// keys survive the tick.
        #[test]
        fn sync_icon_theme_is_noop_when_theme_unchanged() {
            let (mut host, _dir) = test_host();
            host.icon_cache.insert(
                (host.icon_theme.clone(), "roost-cached".to_owned(), 32),
                dummy_artwork(),
            );
            host.panel_paint_key = Some(some_panel_key());
            host.dock_paint_key = Some(some_dock_key());
            host.tiles.settings.icon_theme = host.icon_theme.clone();

            host.sync_icon_theme();

            assert_eq!(host.icon_cache.len(), 1, "cache survives no-op sync");
            assert!(host.panel_paint_key.is_some(), "panel key survives");
            assert!(host.dock_paint_key.is_some(), "dock key survives");
        }

        /// A bogus name resolves to `None` and leaves no entry
        /// behind: misses must not fill the bounded cache.
        #[test]
        fn icon_art_returns_none_for_bogus_name_without_caching() {
            let _lock = ENV_LOCK.lock().expect("env lock");
            let tmp = tempfile::tempdir().expect("tempdir");
            let _env = EnvRestore::install(&tmp);
            let (mut host, _dir) = test_host();
            host.icon_theme = "CacheTheme".to_owned();

            assert_eq!(host.icon_art("no-such-roost-icon", 32), None);
            assert!(
                host.icon_cache.is_empty(),
                "misses must not pollute the cache"
            );
        }

        /// A fixture PNG resolves through the host, and the second
        /// call serves the cached artwork without growing the map.
        #[test]
        fn icon_art_caches_resolved_artwork() {
            let _lock = ENV_LOCK.lock().expect("env lock");
            let tmp = icon_theme_fixture("CacheTheme", "roost-cache-icon");
            let _env = EnvRestore::install(&tmp);
            let (mut host, _dir) = test_host();
            host.icon_theme = "CacheTheme".to_owned();

            let first = host
                .icon_art("roost-cache-icon", 32)
                .expect("fixture icon resolves");
            assert_eq!(first.size, 32);
            assert_eq!(first.argb.len(), 32 * 32 * 4);
            assert_eq!(host.icon_cache.len(), 1);

            let second = host
                .icon_art("roost-cache-icon", 32)
                .expect("cached icon resolves");
            assert_eq!(first, second, "second call serves cached artwork");
            assert_eq!(host.icon_cache.len(), 1, "repeat hit adds no entry");
        }

        /// Host with one pinned entry carrying `icon`: the dock item
        /// under test.
        fn dock_icon_test_host(icon: Option<&str>) -> (ShellHost, tempfile::TempDir) {
            use crate::apps::AppEntry;
            let dir = tempfile::tempdir().expect("tempdir");
            let mut host = ShellHost::new(
                PanelConfig::default(),
                AppProvider::new(vec![AppEntry {
                    app_id: "org.example.True.desktop".to_owned(),
                    name: "True".to_owned(),
                    generic_name: None,
                    keywords: Vec::new(),
                    argv: vec![std::ffi::OsString::from("/bin/true")],
                    icon: icon.map(str::to_owned),
                }]),
                Favorites::load(dir.path().join(crate::favorites::FAVORITES_FILE)),
            );
            host.favorites.pin("org.example.True.desktop");
            (host, dir)
        }

        /// A pinned app's desktop-entry icon resolves through the
        /// host cache at dock size; misses and icon-less entries
        /// keep the fallback.
        #[test]
        fn dock_icon_resolves_pinned_app_through_cache() {
            let _lock = ENV_LOCK.lock().expect("env lock");
            let tmp = icon_theme_fixture("DockTheme", "roost-dock-app");
            let _env = EnvRestore::install(&tmp);
            let (mut host, _dir) = dock_icon_test_host(Some("roost-dock-app"));
            host.icon_theme = "DockTheme".to_owned();
            let items = host.dock_items();
            assert_eq!(items.len(), 1, "pinned app composes one item");

            let art = host
                .dock_icon(&items[0])
                .expect("pinned app artwork resolves");
            assert_eq!(art.size, crate::dock::DOCK_ICON_PX);
            assert_eq!(
                art.argb.len(),
                crate::dock::DOCK_ICON_PX as usize * crate::dock::DOCK_ICON_PX as usize * 4
            );
            assert_eq!(
                host.icon_cache.len(),
                1,
                "resolution goes through the cache"
            );
            let again = host.dock_icon(&items[0]).expect("cached artwork");
            assert_eq!(art, again, "repeat hit serves cached artwork");

            // Icon-less entries and unknown ids keep the fallback.
            let (mut bare, _dir) = dock_icon_test_host(None);
            bare.icon_theme = "DockTheme".to_owned();
            let bare_items = bare.dock_items();
            assert_eq!(bare.dock_icon(&bare_items[0]), None);
            assert!(
                bare.icon_cache.is_empty(),
                "misses must not pollute the cache"
            );
        }

        /// A theme flip on the tick clears dock artwork and arms a
        /// repaint, and the next resolve serves the new theme's art.
        #[test]
        fn dock_repaint_picks_up_new_theme_art_on_flip() {
            use image::Rgba;
            let _lock = ENV_LOCK.lock().expect("env lock");
            let tmp = tempfile::tempdir().expect("tempdir");
            for (theme, pixel) in [
                ("DockFlipA", Rgba([200, 10, 10, 255])),
                ("DockFlipB", Rgba([10, 10, 200, 255])),
            ] {
                let path = tmp.path().join("icons").join(theme).join("32x32/apps");
                std::fs::create_dir_all(&path).expect("mkdirs");
                let mut img = image::RgbaImage::new(32, 32);
                for px in img.pixels_mut() {
                    *px = pixel;
                }
                img.save(path.join("roost-dock-flip.png"))
                    .expect("save png");
            }
            let _env = EnvRestore::install(&tmp);
            let (mut host, _dir) = dock_icon_test_host(Some("roost-dock-flip"));
            host.icon_theme = "DockFlipA".to_owned();
            host.dock_paint_key = Some(some_dock_key());

            let items = host.dock_items();
            let before = host.dock_icon(&items[0]).expect("theme A art");
            assert_eq!(host.icon_cache.len(), 1);

            // The slow tick runs this sync before repainting the
            // dock on the same pass: cache drops, repaint armed.
            host.tiles.settings.icon_theme = "DockFlipB".to_owned();
            host.sync_icon_theme();
            assert!(host.icon_cache.is_empty(), "theme flip drops cached art");
            assert_eq!(host.dock_paint_key, None, "dock repaints on the same tick");

            let after = host.dock_icon(&items[0]).expect("theme B art");
            assert_ne!(before, after, "repaint serves the new theme's artwork");
        }

        /// Tray `ItemInfo` builder (no bus): SNI pixmap words are
        /// `w` x `h` filler bytes of the right length.
        fn tray_info(status: &str, service: &str) -> crate::watcher::ItemInfo {
            crate::watcher::ItemInfo {
                service: service.to_owned(),
                title: "Tray".to_owned(),
                status: status.to_owned(),
                icon_name: String::new(),
                icon_pixmap: Vec::new(),
                attention_name: String::new(),
                attention_pixmap: Vec::new(),
            }
        }

        fn tray_bytes(w: i32, h: i32, seed: u8) -> Vec<u8> {
            vec![seed; w as usize * h as usize * 4]
        }

        /// Attention pixmap wins while the item needs attention.
        #[test]
        fn build_icon_attention_pixmap_wins_when_needs_attention() {
            use crate::watcher::{argb_to_shm, IndicatorIcon};
            let (mut host, _dir) = test_host();
            let normal = tray_bytes(2, 2, 0x11);
            let attention = tray_bytes(1, 1, 0x22);
            let mut info = tray_info("NeedsAttention", "test.tray");
            info.icon_pixmap = vec![(2, 2, normal)];
            info.attention_pixmap = vec![(1, 1, attention.clone())];
            assert_eq!(
                host.build_icon(&info),
                IndicatorIcon::Pixmap {
                    width: 1,
                    height: 1,
                    argb: argb_to_shm(&attention),
                }
            );
        }

        /// Normal pixmap wins while passive, even with a larger
        /// attention pixmap present.
        #[test]
        fn build_icon_normal_pixmap_wins_when_passive() {
            use crate::watcher::{argb_to_shm, IndicatorIcon};
            let (mut host, _dir) = test_host();
            let normal = tray_bytes(2, 2, 0x11);
            let attention = tray_bytes(4, 4, 0x22);
            let mut info = tray_info("Passive", "test.tray");
            info.icon_pixmap = vec![(2, 2, normal.clone())];
            info.attention_pixmap = vec![(4, 4, attention)];
            assert_eq!(
                host.build_icon(&info),
                IndicatorIcon::Pixmap {
                    width: 2,
                    height: 2,
                    argb: argb_to_shm(&normal),
                }
            );
        }

        /// Attention name resolves through the theme fixture at
        /// indicator size when no pixmap applies.
        #[test]
        fn build_icon_attention_name_resolves_via_theme_fixture() {
            use crate::watcher::{IndicatorIcon, INDICATOR_CELL};
            let _lock = ENV_LOCK.lock().expect("env lock");
            let tmp = icon_theme_fixture("TrayTheme", "roost-tray-attention");
            let _env = EnvRestore::install(&tmp);
            let (mut host, _dir) = test_host();
            host.icon_theme = "TrayTheme".to_owned();
            let mut info = tray_info("NeedsAttention", "test.tray");
            info.attention_name = "roost-tray-attention".to_owned();
            match host.build_icon(&info) {
                IndicatorIcon::Pixmap {
                    width,
                    height,
                    argb,
                } => {
                    assert_eq!((width, height), (INDICATOR_CELL, INDICATOR_CELL));
                    assert_eq!(
                        argb.len(),
                        INDICATOR_CELL as usize * INDICATOR_CELL as usize * 4
                    );
                }
                other => panic!("attention name must resolve, got {other:?}"),
            }
        }

        /// Unknown (and empty) names fall through to the service
        /// fallback; misses must not pollute the artwork cache.
        #[test]
        fn build_icon_unknown_names_fall_back_to_named_service() {
            use crate::watcher::IndicatorIcon;
            let _lock = ENV_LOCK.lock().expect("env lock");
            let tmp = tempfile::tempdir().expect("tempdir");
            let _env = EnvRestore::install(&tmp);
            let (mut host, _dir) = test_host();
            host.icon_theme = "TrayTheme".to_owned();
            let mut info = tray_info("Active", "test.tray");
            info.icon_name = "no-such-roost-tray-icon".to_owned();
            info.attention_name = "no-such-roost-tray-attention".to_owned();
            assert_eq!(
                host.build_icon(&info),
                IndicatorIcon::Named("test.tray".to_owned())
            );
            let empty = tray_info("NeedsAttention", "test.tray");
            assert_eq!(
                host.build_icon(&empty),
                IndicatorIcon::Named("test.tray".to_owned())
            );
            assert!(
                host.icon_cache.is_empty(),
                "name misses must not pollute the cache"
            );
        }

        /// A `-symbolic` name re-tints toward the shell accent: RGB
        /// becomes [`crate::overview::ACCENT`], alpha is preserved.
        #[test]
        fn build_icon_symbolic_name_retints_toward_accent() {
            use crate::watcher::IndicatorIcon;
            let _lock = ENV_LOCK.lock().expect("env lock");
            let tmp = icon_theme_fixture("TrayTheme", "roost-tray-symbolic");
            let _env = EnvRestore::install(&tmp);
            let (mut host, _dir) = test_host();
            host.icon_theme = "TrayTheme".to_owned();
            let mut info = tray_info("Active", "test.tray");
            info.icon_name = "roost-tray-symbolic".to_owned();
            match host.build_icon(&info) {
                IndicatorIcon::Pixmap { argb, .. } => {
                    assert!(!argb.is_empty(), "symbolic icon must decode");
                    let accent = crate::overview::ACCENT;
                    let (chunks, _) = argb.as_chunks::<4>();
                    for px in chunks {
                        assert_eq!(&px[0..3], &accent[0..3], "RGB re-tints to ACCENT");
                        // The fixture PNG is fully opaque.
                        assert_eq!(px[3], 255, "alpha is preserved");
                    }
                }
                other => panic!("symbolic name must resolve, got {other:?}"),
            }
        }

        /// Menu preservation across `poll_indicators` is not driven
        /// headless: without a bus the poll returns early, and with
        /// one it would prune the host via `retain_registered`, so
        /// the carry (`get` + `upsert`) needs the live-bus harness.
        /// This pins the documented precondition instead: a
        /// disconnected edge yields no info, in which case a poll
        /// keeps whatever the host already holds.
        #[test]
        fn poll_menu_preservation_needs_live_bus_not_driven_headless() {
            let bus = crate::watcher::WatcherBus::new();
            assert_eq!(bus.fetch_info("test.indicator"), None);
            assert!(bus.registered().is_empty());
        }
    }
}
