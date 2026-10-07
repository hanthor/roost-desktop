//! Floating window management with seat input routing (001 T2).
//!
//! [`WindowManager`] joins live xdg toplevels to the compositor-owned
//! [`StateModel`]: mapping and unmapping, click- and motion-driven focus,
//! keyboard and pointer delivery, programmatic move/resize on one
//! workspace, and title synchronization. Interactive drag-to-move is
//! deliberately out of this slice; geometry changes go through the
//! manager API so every mutation flows through the model.
//!
//! Mapping behavior follows the surveyed references (niri/cosmic-comp in
//! `docs/reference-repos.md`): a new toplevel is registered, configured,
//! and focused; titles are read from the surface role data; stacking is
//! insertion order with focus raising to top. All code here is original.

use std::collections::HashMap;

#[cfg(feature = "xwayland")]
use crate::xwayland::{map_x11_identity, X11ManagerEvent};
use smithay::reexports::wayland_server::Resource as _;
#[cfg(feature = "xwayland")]
use smithay::xwayland::{xwm::WmWindowType, X11Surface};
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, ButtonState, Event as BackendEvent, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{Window, WindowSurface},
    input::{
        keyboard::{FilterResult, KeyboardHandle, XkbConfig},
        pointer::{AxisFrame, ButtonEvent, MotionEvent, PointerHandle},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::IsAlive,
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        seat::WaylandFocus,
        shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData},
    },
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;

use crate::{
    state::{StateModel, WindowUpdate},
    State, WindowRequest,
};
use roost_shell_control::{switcher_keys, SwitcherAction, PANEL_HEIGHT};

/// Default floating size for a newly mapped window.
const DEFAULT_WIDTH: i32 = 800;
const DEFAULT_HEIGHT: i32 = 600;
/// Cascade offset for each newly mapped window.
const CASCADE_STEP: i32 = 50;

/// One managed window: live surface plus compositor-side geometry.
/// The surface is the toolkit's unified window handle, so native and
/// X11 windows share one entry shape; only configure/close/identity
/// branch on the underlying kind.
#[derive(Debug, Clone)]
struct ManagedWindow {
    surface: Window,
    geometry: Rectangle<i32, Logical>,
    /// Current presentation layout (floating, maximized, tiled, or
    /// fullscreen). Manager-local like geometry: the shell never sees
    /// it, it only sees the resulting geometry through rendering.
    layout: WindowLayout,
    /// Geometry to restore when leaving a non-floating layout. Stashed
    /// on leaving floating, cleared by manual move/resize (a fresh
    /// start, GNOME shape).
    restore: Option<Rectangle<i32, Logical>>,
    /// Strip width-preset slot (`STRIP_PRESETS` index) chosen with
    /// Super+R; `None` means the default. Survives mode toggles.
    preset: Option<usize>,
    /// Not placed yet: the client picks its own size from an empty
    /// configure and is placed at that size on its first commit, as
    /// Mutter does.
    unplaced: bool,
    /// Hidden (GNOME's minimize): off the screen and out of focus, still
    /// in the overview and Alt+Tab; activating it brings it back.
    minimized: bool,
    /// Always on Top: stacked above every window without it.
    above: bool,
    /// Always on Visible Workspace: shown on every workspace.
    sticky: bool,
    /// Trusted cross-backend modal parent, pinned to a managed generation.
    x11_parent: Option<u64>,
}

/// Compositor-side presentation layout (002 window actions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowLayout {
    /// Normal floating window at its own geometry.
    #[default]
    Floating,
    /// Fills the work area (output minus the panel strip).
    Maximized,
    /// Fills one work-area half.
    Tiled(TileSide),
    /// Covers the whole output, including the panel strip.
    Fullscreen,
    /// One column of the scroll-mode strip. The session [`SessionMode`]
    /// flag owns whether the strip is active; this variant is only the
    /// stash vehicle so entering the strip preserves floating geometry
    /// through the existing stash-once/restore path.
    Strip,
}

/// What a moved window dropped at a point snaps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    /// The top edge: maximize.
    Maximize,
    /// A side edge: tile that half.
    Tile(TileSide),
}

/// Where a window dropped with the pointer at `pos` snaps on an output
/// `width` wide (GNOME edge tiling), if anywhere.
pub fn snap_target(pos: Point<f64, Logical>, width: i32) -> Option<Snap> {
    if pos.y <= f64::from(WORK_AREA_TOP) + SNAP_EDGE_PX {
        Some(Snap::Maximize)
    } else if pos.x <= SNAP_EDGE_PX {
        Some(Snap::Tile(TileSide::Left))
    } else if pos.x >= f64::from(width) - 1.0 - SNAP_EDGE_PX {
        Some(Snap::Tile(TileSide::Right))
    } else {
        None
    }
}

/// Which work-area half a tiled window fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileSide {
    /// Left half.
    Left,
    /// Right half.
    Right,
}

/// Session-wide window-management mode (scrollable-tiling spec).
/// Manager-local like [`WindowLayout`]: the shell never sees it, it
/// only sees resulting geometry through rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionMode {
    /// Normal floating desktop (the default).
    #[default]
    Gnome,
    /// Horizontal scrollable strip; every window is a column.
    Scroll,
}

/// Floating window manager over one workspace.
///
/// Owns the [`StateModel`] and the surface↔model index. The [`State`]
/// protocol object stays the caller: every method takes the seat handles
/// it needs from `state`, so input delivery, focus, and configure
/// round-trips share one call path for live events and tests.
/// Interactive pointer grab on one window (#58).
#[derive(Debug, Clone, Copy, PartialEq)]
enum PointerGrab {
    /// Move: the window follows the pointer from where the drag began.
    Move {
        id: u64,
        pointer_start: Point<f64, Logical>,
        loc_start: Point<i32, Logical>,
    },
    /// Resize from `edges` (xdg ResizeEdge bits).
    Resize {
        id: u64,
        edges: u32,
        pointer_start: Point<f64, Logical>,
        geometry_start: Rectangle<i32, Logical>,
    },
}

/// xdg_toplevel ResizeEdge bits.
const EDGE_TOP: u32 = 1;
const EDGE_BOTTOM: u32 = 2;
const EDGE_LEFT: u32 = 4;
const EDGE_RIGHT: u32 = 8;
/// Smallest size an interactive resize may reach.
pub const MIN_WINDOW_SIZE: (i32, i32) = (120, 80);
/// Distance from the top/side edge that snaps a dropped window.
pub const SNAP_EDGE_PX: f64 = 2.0;

/// New rectangle for a resize from `edges` by `(dx, dy)`, never smaller
/// than [`MIN_WINDOW_SIZE`]; the opposite edges stay put.
pub fn resized(
    start: Rectangle<i32, Logical>,
    edges: u32,
    dx: i32,
    dy: i32,
) -> Rectangle<i32, Logical> {
    let (mut x, mut y, mut w, mut h) = (start.loc.x, start.loc.y, start.size.w, start.size.h);
    if edges & EDGE_RIGHT != 0 {
        w = (start.size.w + dx).max(MIN_WINDOW_SIZE.0);
    }
    if edges & EDGE_BOTTOM != 0 {
        h = (start.size.h + dy).max(MIN_WINDOW_SIZE.1);
    }
    if edges & EDGE_LEFT != 0 {
        w = (start.size.w - dx).max(MIN_WINDOW_SIZE.0);
        x = start.loc.x + start.size.w - w;
    }
    if edges & EDGE_TOP != 0 {
        h = (start.size.h - dy).max(MIN_WINDOW_SIZE.1);
        y = start.loc.y + start.size.h - h;
    }
    Rectangle::new((x, y).into(), (w, h).into())
}

pub struct WindowManager {
    /// Active interactive move/resize, if any (#58).
    grab: Option<PointerGrab>,
    /// Window focused before it had a surface to focus (an X11 window
    /// mapped before association): the seat follows once it does.
    focus_awaits_surface: Option<u64>,
    /// Button whose press dismissed a popup grab: its release is
    /// swallowed too, so the window beneath never sees half a click.
    swallowed_button: Option<u32>,
    model: StateModel,
    windows: HashMap<u64, ManagedWindow>,
    surface_index: HashMap<WlSurface, u64>,
    /// X11 window id → model id, for the X11 ingest path. A unified
    /// `Window` mints a fresh id on every construction, so ingest
    /// keeps keying on the underlying surfaces while every managed
    /// entry holds the one unified handle.
    #[cfg(feature = "xwayland")]
    x11_index: HashMap<u32, u64>,
    /// Model ids bottom-to-top; focus raises to the top.
    stacking: Vec<u64>,
    cascade: i32,
    keyboard: Option<KeyboardHandle<State>>,
    pointer: Option<PointerHandle<State>>,
    pointer_pos: Point<f64, Logical>,
    /// org.gnome.Shell accelerator grabs (GrabAccelerators).
    accelerators: Vec<roost_shell_control::Accelerator>,
    /// Keycodes whose press fired an accelerator: their release is
    /// swallowed too.
    accel_held: Vec<u32>,
    /// Fired accelerators `(action, time, mode)`, drained by the runtime.
    accel_fired: Vec<(u32, u32, u32)>,
    /// Input is going to the session-lock surface.
    lock_input_active: bool,
    /// Window-menu requests `(window, x, y)` in global logical pixels,
    /// drained by the runtime for the shell.
    menu_requests: Vec<(u64, i32, i32)>,
    /// The active workspace at the last reconcile.
    last_active_workspace: Option<u32>,
    /// The modifiers of the chord that opened the switcher, Shift
    /// aside: releasing them all commits (GNOME's modifier mask).
    switcher_opener: u32,
    /// The shell's switcher chords (GNOME's `switch-applications` and
    /// `switch-group` keys); `None` until it sends them, and then the
    /// built-in Alt/Super+Tab and Above_Tab stand aside.
    switcher_keys: Option<Vec<roost_shell_control::SwitcherKey>>,
    /// Control held (either side), for switcher chords.
    ctrl_held: bool,
    /// Workspace-switcher popups `(index, count)` for the shell.
    workspace_popups: Vec<(u32, u32)>,
    /// Super held (either side) for workspace keybindings.
    super_held: bool,
    /// Key repeat as last set: rate (keys/s, 0 off) and delay (ms).
    repeat: (i32, i32),
    /// Shift held (either side) for move-window keybindings.
    shift_held: bool,
    /// Alt held (either side) for the Alt-Tab switcher.
    alt_held: bool,
    /// Whether an Alt-Tab switcher session is open (Alt held past a
    /// Tab tap). Guards Escape-cancel consumption and Alt-release
    /// commit so plain Escape/Alt keep reaching clients.
    switcher_open: bool,
    /// Switcher drive events queued for the hub broadcast, drained by
    /// the runtime after each input event.
    switcher_queue: Vec<SwitcherAction>,
    /// Keys the open switcher took: their releases stay invisible too.
    switcher_swallowed: Vec<u32>,
    /// Session window-management mode (scrollable-tiling spec).
    /// Gnome by default; Super+Shift+T flips the whole session.
    mode: SessionMode,
    /// Horizontal strip view offset in logical pixels (scroll mode).
    /// Zero on entering scroll; clamped to the strip overflow. This is
    /// the view's *target*: layout, configure sizes, input and tests
    /// all use it, so clients are configured once at the target.
    strip_offset: f64,
    /// Where the strip view is drawn right now: chases `strip_offset`
    /// on niri's view-movement spring, stepped once per frame by
    /// [`step_strip_view`](Self::step_strip_view). Only render
    /// positions use it.
    strip_shown: f64,
    animations_enabled: bool,
    column_animations: HashMap<u64, crate::animation::ColumnSpring>,
    /// The running view spring and its elapsed seconds, while the drawn
    /// view has not settled on the target.
    strip_anim: Option<(crate::spring::Spring, f64)>,
    /// Last hub overview flag seen (set by the runtime each tick).
    overview_open: bool,
    /// Window focused before the overview parked keyboard focus.
    pre_overview_focus: Option<u64>,
    /// The window left activated while the overview holds the keyboard
    /// (GNOME keeps the focused window's activated look there).
    overview_activated: Option<u64>,
    /// The window this manager last told it was activated. The shell's
    /// ActivateWindow sets the model's focus before the manager applies
    /// it, so the model cannot say which window to deactivate.
    activated: Option<u64>,
    /// Overview surface keyboard focus is parked on, if parked.
    overview_held: Option<WlSurface>,
    /// An exclusive-keyboard layer surface holding the keyboard (a
    /// modal dialog), released back to the focused window on unmap.
    exclusive_held: Option<WlSurface>,
}

impl WindowManager {
    /// Last known pointer position, for position-less events (button
    /// presses) that need a location, e.g. the Activities-strip trigger.
    pub fn pointer_pos(&self) -> Point<f64, Logical> {
        self.pointer_pos
    }

    /// Empty manager; attaches keyboard and pointer capabilities to the
    /// state's seat and keeps their handles for input routing.
    pub fn new(state: &mut State) -> Self {
        let seat = state.seat_mut();
        let keyboard = seat.add_keyboard(XkbConfig::default(), 200, 200).ok();
        let pointer = Some(seat.add_pointer());
        Self {
            model: StateModel::new(),
            windows: HashMap::new(),
            surface_index: HashMap::new(),
            #[cfg(feature = "xwayland")]
            x11_index: HashMap::new(),
            stacking: Vec::new(),
            cascade: 0,
            keyboard,
            pointer,
            pointer_pos: (0.0, 0.0).into(),
            accelerators: Vec::new(),
            accel_held: Vec::new(),
            accel_fired: Vec::new(),
            lock_input_active: false,
            menu_requests: Vec::new(),
            last_active_workspace: None,
            switcher_opener: 0,
            switcher_keys: None,
            ctrl_held: false,
            workspace_popups: Vec::new(),
            swallowed_button: None,
            focus_awaits_surface: None,
            grab: None,
            mode: SessionMode::Gnome,
            strip_offset: 0.0,
            strip_shown: 0.0,
            animations_enabled: true,
            column_animations: HashMap::new(),
            strip_anim: None,
            super_held: false,
            // add_keyboard below: 200 ms delay, 200 keys/s.
            repeat: (200, 200),
            shift_held: false,
            alt_held: false,
            switcher_open: false,
            switcher_queue: Vec::new(),
            switcher_swallowed: Vec::new(),
            overview_open: false,
            pre_overview_focus: None,
            overview_activated: None,
            activated: None,
            overview_held: None,
            exclusive_held: None,
        }
    }

    /// Record the hub overview flag (the runtime calls this each tick
    /// after polling control). The next [`reconcile`](Self::reconcile)
    /// steers focus.
    pub fn set_overview_open(&mut self, open: bool) {
        self.overview_open = open;
    }

    /// Whether keyboard focus is currently parked on the overview.
    pub fn overview_focus_held(&self) -> bool {
        self.overview_held.is_some()
    }

    /// Compositor-owned state model (revisioned windows/focus).
    pub fn model(&self) -> &StateModel {
        &self.model
    }

    /// Mutable model for control-command application.
    pub fn model_mut(&mut self) -> &mut StateModel {
        &mut self.model
    }

    /// Every mapped window on every workspace, bottom to top, for the
    /// overview (#54).
    pub fn overview_windows(&self) -> Vec<crate::overview::OverviewWindow> {
        self.stacking
            .iter()
            .filter_map(|id| {
                let entry = self.model.window(*id)?;
                let window = self.windows.get(id)?;
                Some(crate::overview::OverviewWindow {
                    id: *id,
                    // A sticky window sits on whichever workspace shows.
                    workspace: if window.sticky {
                        self.model.active_workspace()
                    } else {
                        entry.workspace
                    },
                    geometry: window.geometry,
                })
            })
            .collect()
    }

    /// The Wayland surface of window `id`, if it has one.
    pub fn surface_of(&self, id: u64) -> Option<WlSurface> {
        self.windows
            .get(&id)
            .and_then(|w| w.surface.wl_surface())
            .map(|s| s.into_owned())
    }

    /// GNOME considers an application standalone if one of its windows has
    /// no live transient parent. Read actual role/X11 metadata, not app-id text.
    pub fn is_standalone(&self, id: u64) -> bool {
        if self
            .windows
            .get(&id)
            .and_then(|w| w.x11_parent)
            .is_some_and(|parent| self.windows.contains_key(&parent))
        {
            return false;
        }
        self.windows
            .get(&id)
            .is_some_and(|window| match window.surface.underlying_surface() {
                WindowSurface::Wayland(toplevel) => {
                    !read_parent(toplevel).is_some_and(|parent| parent.is_alive())
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(surface) => !surface
                    .is_transient_for()
                    .is_some_and(|parent| self.x11_index.contains_key(&parent)),
            })
    }

    /// GNOME assigns a transient window to its ultimate mapped parent app,
    /// including imported cross-client parents. Cycles fall back to the
    /// original window; never recurse through untrusted role metadata.
    pub fn application_window(&self, id: u64) -> Option<u64> {
        let mut current = id;
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(current) {
                return self.windows.contains_key(&id).then_some(id);
            }
            self.windows.get(&current)?;
            let parent = self.transient_parent(current);
            match parent {
                Some(parent) => current = parent,
                None => return Some(current),
            }
        }
    }

    /// Shared parent lookup for placement, modality and app association.
    /// External relationships retain model IDs rather than reusable XIDs.
    pub fn transient_parent(&self, id: u64) -> Option<u64> {
        let window = self.windows.get(&id)?;
        if let Some(parent) = window.x11_parent.filter(|p| self.windows.contains_key(p)) {
            return Some(parent);
        }
        match window.surface.underlying_surface() {
            WindowSurface::Wayland(toplevel) => read_parent(toplevel)
                .filter(|p| p.is_alive())
                .and_then(|p| {
                    self.windows.iter().find_map(|(id, w)| {
                        (w.surface.wl_surface().as_deref() == Some(&p)).then_some(*id)
                    })
                }),
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(surface) => surface
                .is_transient_for()
                .and_then(|xid| self.x11_index.get(&xid).copied()),
        }
    }

    #[cfg(feature = "xwayland")]
    fn reconcile_x11_parents(&mut self, state: &mut State) {
        let requests = std::mem::take(&mut state.protocols.x11_interop.requests);
        for (surface, parent) in requests {
            if state.protocols.shortcut_locked || !surface.is_alive() {
                continue;
            }
            let Some(child) = self.surface_index.get(&surface).copied() else {
                continue;
            };
            if child == parent || !self.is_x11(parent) {
                continue;
            }
            let parent_live = self
                .windows
                .get(&parent)
                .is_some_and(|window| window.surface.alive());
            if !parent_live {
                continue;
            }
            if let Some(window) = self.windows.get_mut(&child) {
                window.x11_parent = Some(parent);
            }
            self.place_above(parent, child);
            if let Some(geometry) = self.geometry(child) {
                state.window_origins.insert(surface.clone(), geometry.loc);
                state.refresh_initial_surface_scale(&surface);
            }
            if let Some(workspace) = self.model.window(parent).map(|w| w.workspace) {
                self.move_to_workspace(state, child, workspace);
            }
            if self.model.focused() == Some(parent) {
                self.apply_focus(state, Some(parent));
            }
        }
        // Publish the original X11 objects, including managed parents whose
        // Wayland buffer has not yet associated. The request path checks life
        // and captures the model generation before deferred scene mutation.
        state.protocols.x11_interop.parents = self
            .x11_index
            .iter()
            .filter_map(|(xid, id)| {
                let window = self.windows.get(id)?;
                match window.surface.underlying_surface() {
                    WindowSurface::X11(surface)
                        if surface.alive() && !surface.is_override_redirect() =>
                    {
                        Some((*xid, (*id, surface.clone())))
                    }
                    _ => None,
                }
            })
            .collect();
    }

    /// GNOME's GetWindows picker allows ordinary toplevels and dialogs,
    /// but excludes X11 notification/menu/tooltip/splash/toolbar roles.
    /// Read live XWM metadata so a property change updates the next snapshot.
    pub fn is_introspect_eligible(&self, id: u64) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|window| match window.surface.underlying_surface() {
                WindowSurface::Wayland(toplevel) => toplevel.wl_surface().is_alive(),
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(surface) => {
                    !surface.is_override_redirect() && introspect_x11_type(surface.window_type())
                }
            })
    }

    /// Actual X11 resource identity for diagnostic journeys, never authority.
    pub fn x11_window_id(&self, id: u64) -> Option<u32> {
        #[cfg(feature = "xwayland")]
        if let Some(window) = self.windows.get(&id) {
            if let WindowSurface::X11(surface) = window.surface.underlying_surface() {
                return Some(surface.window_id());
            }
        }
        let _ = id;
        None
    }

    /// Whether window `id` is an X11 window (through Xwayland).
    pub fn is_x11(&self, id: u64) -> bool {
        #[cfg(feature = "xwayland")]
        {
            self.windows
                .get(&id)
                .is_some_and(|w| matches!(w.surface.underlying_surface(), WindowSurface::X11(_)))
        }
        #[cfg(not(feature = "xwayland"))]
        {
            let _ = id;
            false
        }
    }

    /// Switch to `workspace` by id, if it exists.
    pub fn switch_to_workspace(&mut self, state: &mut State, workspace: u32) -> bool {
        let list = self.model.workspaces().to_vec();
        let (Some(from), Some(to)) = (
            list.iter()
                .position(|w| *w == self.model.active_workspace()),
            list.iter().position(|w| *w == workspace),
        ) else {
            return false;
        };
        self.switch_relative(state, to as i32 - from as i32)
    }

    /// Windows bottom-to-top with their geometry, for frame production.
    /// Only the active workspace renders; other workspaces keep their
    /// surfaces mapped but hidden.
    pub fn visible_windows(&self) -> Vec<(Window, Rectangle<i32, Logical>)> {
        self.visible_entries()
            .into_iter()
            .map(|(_, window, geometry)| (window, geometry))
            .collect()
    }

    /// [`visible_windows`](Self::visible_windows) where they are drawn
    /// this frame: column widths and positions spring to their targets,
    /// shifted by how far the animated view still trails its target. Every other use (input, configure,
    /// output scale) keeps the target geometry.
    pub fn render_windows(&self) -> Vec<(Window, Rectangle<i32, Logical>)> {
        self.visible_entries()
            .into_iter()
            .map(|(id, window, geometry)| (window, self.shifted(id, geometry)))
            .collect()
    }

    /// Where `id` is drawn this frame (see
    /// [`render_windows`](Self::render_windows)).
    pub fn render_geometry(&self, id: u64) -> Option<Rectangle<i32, Logical>> {
        self.geometry(id).map(|geometry| self.shifted(id, geometry))
    }

    /// `geometry` moved by the strip view's lag when `id` is a strip
    /// column: drawn x = target x + (target offset - drawn offset).
    fn shifted(&self, id: u64, mut geometry: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
        if self.mode == SessionMode::Scroll && self.window_layout(id) == Some(WindowLayout::Strip) {
            if let Some(animation) = self.column_animations.get(&id) {
                geometry = animation.value();
                geometry.loc.x -= self.strip_shown.round() as i32;
            } else {
                geometry.loc.x += (self.strip_offset - self.strip_shown).round() as i32;
            }
        }
        geometry
    }

    /// Visible windows bottom-to-top with their ids and target geometry.
    fn visible_entries(&self) -> Vec<(u64, Window, Rectangle<i32, Logical>)> {
        let active = self.model.active_workspace();
        self.stacking
            .iter()
            .copied()
            .filter(|id| {
                self.model
                    .window(*id)
                    .is_some_and(|entry| entry.workspace == active)
                    || self.windows.get(id).is_some_and(|w| w.sticky)
            })
            .filter_map(|id| {
                self.windows
                    .get(&id)
                    .filter(|w| !w.minimized)
                    .map(|w| (id, w.surface.clone(), w.geometry))
            })
            .collect()
    }

    /// Whether the pointer is over a visible fullscreen window. Hidden or
    /// minimized windows and fullscreen windows on another output do not
    /// suppress that output's hot corner.
    pub fn fullscreen_at(&self, pos: Point<f64, Logical>) -> bool {
        self.visible_entries().iter().any(|(id, _, geometry)| {
            self.windows
                .get(id)
                .is_some_and(|window| window.layout == WindowLayout::Fullscreen)
                && geometry.to_f64().contains(pos)
        })
    }

    /// Wayland surfaces of mapped X11 windows, for frame production.
    /// The state's toplevel list only covers native windows, and
    /// Xwayland paces its content commits on frame callbacks, so
    /// without these an X11 window presents nothing and renders
    /// invisible. (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    pub fn x11_surfaces(&self) -> Vec<WlSurface> {
        self.windows
            .values()
            .filter(|window| matches!(window.surface.underlying_surface(), WindowSurface::X11(_)))
            .filter_map(|window| {
                window
                    .surface
                    .wl_surface()
                    .map(|surface| surface.into_owned())
            })
            .collect()
    }

    /// Reconcile live surfaces with the model: map new toplevels, drop
    /// dead ones, sync titles, and drain client window-state requests.
    /// Called once per loop tick after client dispatch, and directly
    /// by tests.
    #[cfg(feature = "xwayland")]
    pub(crate) fn x11_icon_identities(&self) -> Vec<(u32, u64)> {
        self.x11_index.iter().map(|(x, id)| (*x, *id)).collect()
    }

    pub fn reconcile(&mut self, state: &mut State) {
        // Popups (#88): drop dead trees, and once the last grabbed popup
        // is gone hand keyboard focus back to the focused window.
        state.popups.cleanup();
        state.window_origins = self
            .visible_windows()
            .into_iter()
            .filter_map(|(w, g)| w.wl_surface().map(|s| (s.into_owned(), g.loc)))
            .collect();
        state.refresh_ime_cursor_origins();
        // Never pull focus out from under a live grab.
        if state.take_popup_refocus() && !state.popup_grab_active() {
            let focused = self.model.focused();
            self.apply_focus(state, focused);
            // The grab held pointer focus wherever the pointer went:
            // re-deliver it where the pointer is now, so the surface it
            // left hears the leave (a panel button drops its hover).
            if self.pointer.is_some() {
                let pos = self.pointer_pos;
                let time = (crate::state::system_millis() & u64::from(u32::MAX)) as u32;
                self.pointer_motion(state, pos, time);
            }
        }
        let live: Vec<ToplevelSurface> = state.toplevels();
        let mut seen = Vec::with_capacity(live.len());
        for surface in &live {
            let wl = surface.wl_surface().clone();
            match self.surface_index.get(&wl).copied() {
                Some(id) => {
                    seen.push(id);
                    self.sync_title(id, surface);
                }
                None => {
                    seen.push(self.map(state, surface));
                }
            }
        }
        for (surface, id) in &self.surface_index {
            self.model.set_icon(*id, state.window_icons.get(surface));
        }
        // Liveness runs over the unified handles: `Window::alive`
        // delegates per variant, so native and X11 windows drop on
        // the same path.
        let gone: Vec<u64> = self
            .windows
            .iter()
            .filter(|(_, window)| !window.surface.alive())
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.unmap(state, id);
        }
        // Discard destruction records for roles that never entered the model.
        state.closed_toplevel_parents.clear();
        #[cfg(feature = "xwayland")]
        self.drain_x11_events(state);
        #[cfg(feature = "xwayland")]
        self.reconcile_x11_parents(state);
        // Smithay's Window geometry uses a cached surface-tree bounding box.
        // Refresh it after dispatched commits (including unsynchronized
        // subsurfaces) before render-time width scaling reads the geometry.
        // A newly constructed Window otherwise keeps its zero-size bbox.
        for window in self.windows.values() {
            window.surface.on_commit();
        }
        self.place_committed(state);
        // Focus tracks the active workspace: a model-level switch (e.g.
        // shell FocusWorkspace) that strands focus on a hidden window
        // refocuses the new topmost here, converging seat focus and
        // Activated states on the next tick.
        let active = self.model.active_workspace();
        let stranded = self
            .model
            .focused()
            .and_then(|id| self.model.window(id))
            .is_some_and(|entry| entry.workspace != active);
        // Arriving on a workspace with nothing focused focuses its top
        // window, as GNOME does, whichever path switched (keys, shell).
        let arrived = self.last_active_workspace != Some(active) && self.model.focused().is_none();
        self.last_active_workspace = Some(active);
        if stranded || arrived {
            self.focus_topmost(state, active);
        }
        // Client window-state requests drain last so a request that
        // arrived with a surface's first commits (e.g. initial
        // maximized) applies after the surface is mapped.
        self.drain_window_requests(state);
        self.drain_pointer_warps(state);
        #[cfg(feature = "xwayland")]
        self.sync_seat_focus(state);
        // Overview focus parks last so newly mapped windows never
        // hold focus past this tick while the overview is open.
        self.reconcile_overview_focus(state);
        self.reconcile_exclusive_layer(state);
        let _ = seen;
    }

    /// Park keyboard (and selection) focus on the shell overview while
    /// open; restore the previous window on dismiss. An explicit `focus`
    /// while parked chooses the window restored on close. A vanished
    /// overview surface restores early so keys never route into a dead
    /// surface.
    fn reconcile_overview_focus(&mut self, state: &mut State) {
        let surface = state.overview_surface();
        if self.overview_open {
            match (surface, self.overview_held.clone()) {
                (Some(target), held) if held.as_ref() != Some(&target) => {
                    if held.is_none() {
                        self.pre_overview_focus = self.model.focused();
                        // Mutter keeps the focus window while the stage
                        // holds the keys: it stays activated.
                        self.overview_activated = self.model.focused();
                    }
                    self.model.set_focused(None);
                    let serial = SERIAL_COUNTER.next_serial();
                    if let Some(keyboard) = self.keyboard.clone() {
                        keyboard.set_focus(state, Some(target.clone()), serial);
                    }
                    state.sync_selection_focus(Some(&target));
                    eprintln!("roost-compositor: overview focus parked");
                    self.overview_held = Some(target);
                }
                (None, Some(_)) => {
                    self.restore_pre_overview_focus(state);
                }
                _ => {}
            }
        } else if self.overview_held.is_some() {
            self.restore_pre_overview_focus(state);
        }
    }

    /// Restore the pre-overview window (or the topmost live one) after
    /// dismiss or surface loss.
    /// wlr-layer-shell keyboard exclusivity: while a top or overlay
    /// surface asking for it is mapped, it holds the keyboard (GNOME's
    /// modal dialogs grab it the same way); on unmap the focused window
    /// gets it back. The overview park takes precedence while open.
    fn reconcile_exclusive_layer(&mut self, state: &mut State) {
        if self.overview_held.is_some() {
            return;
        }
        let wanted = crate::layer::exclusive_keyboard_layer(state);
        if wanted == self.exclusive_held {
            return;
        }
        let serial = SERIAL_COUNTER.next_serial();
        match wanted.clone() {
            Some(surface) => {
                if let Some(keyboard) = self.keyboard.clone() {
                    keyboard.set_focus(state, Some(surface.clone()), serial);
                }
                state.sync_selection_focus(Some(&surface));
            }
            None => {
                let focused = self.model.focused();
                self.apply_focus(state, focused);
            }
        }
        self.exclusive_held = wanted;
    }

    fn restore_pre_overview_focus(&mut self, state: &mut State) {
        self.overview_held = None;
        let restore = self
            .pre_overview_focus
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.stacking.last().copied());
        self.pre_overview_focus = None;
        if let Some(old) = self.overview_activated.take() {
            if Some(old) != restore {
                self.configure(old, false);
            }
        }
        self.apply_focus(state, restore);
        eprintln!("roost-compositor: overview focus restored");
    }

    /// Register one toplevel: model insert with cascaded geometry,
    /// initial configure, and focus. Returns the model id. The
    /// unified handle is constructed once here and stored; ingest
    /// never rebuilds it (a fresh construction mints a new id).
    fn map(&mut self, state: &mut State, surface: &ToplevelSurface) -> u64 {
        let title = read_title(surface).unwrap_or_else(|| "untitled".to_owned());
        let app_id = read_app_id(surface);
        let window = Window::new_wayland_window(surface.clone());
        let id = self.insert_managed(&title, app_id.as_deref());
        self.surface_index.insert(surface.wl_surface().clone(), id);
        let target = state
            .initial_outputs
            .get(surface.wl_surface())
            .and_then(|name| state.outputs.iter().find(|entry| &entry.name == name))
            .or_else(|| state.new_window_output())
            .map(|entry| Rectangle::new(entry.loc.into(), entry.size));
        let geometry = self.placement_on(state, None, target);
        // With a real output the client chooses its size first (Mutter's
        // empty initial configure); headless tests keep the cascade.
        let unplaced = Self::work_area(state).size.w > 0;
        self.windows.insert(
            id,
            ManagedWindow {
                surface: window,
                geometry,
                layout: WindowLayout::Floating,
                restore: None,
                preset: None,
                unplaced,
                minimized: false,
                above: false,
                sticky: false,
                x11_parent: None,
            },
        );
        self.place_transient(surface, id);
        self.finish_map(state, id);
        id
    }

    /// Shared tail of every map path (native and X11): stacking slot,
    /// scroll-mode membership, initial configure, and focus.
    fn finish_map(&mut self, state: &mut State, id: u64) {
        // niri opens a new column right of the focused one; floating
        // windows go on top.
        let after_focused = (self.mode == SessionMode::Scroll)
            .then(|| self.model.focused())
            .flatten()
            .and_then(|focused| self.stacking.iter().position(|other| *other == focused));
        match after_focused {
            Some(index) => self.stacking.insert(index + 1, id),
            None => self.stacking.push(id),
        }
        // Forced strip membership: windows mapped mid-scroll join the
        // strip as columns. Dialogs keep their after-parent stacking
        // slot, so they land in the adjacent column — the strip is
        // their rule instead of floating centering.
        if self.mode == SessionMode::Scroll {
            self.apply_layout(state, id, WindowLayout::Strip);
        }
        self.configure(id, true);
        // A window mapping while the overview holds focus (launched
        // from search or the dash) is the one focused when it closes,
        // as in GNOME; otherwise the pre-overview window would win.
        if self.overview_held.is_some() {
            self.focus_parked(Some(id));
        }
        self.apply_focus(state, Some(id));
    }

    /// Where a new floating window goes. With a real output this follows
    /// GNOME's automatic placement: the first window on an empty
    /// workspace is centered in the work area, later ones cascade from
    /// the most recent by one step, kept inside the work area (never
    /// under the top bar). Without an output (headless tests) the plain
    /// cascade from the origin stands.
    /// Automatic placement for a window that brings its own
    /// size (X11 clients ask for one at map); `None` uses the default.
    #[cfg(feature = "xwayland")]
    fn placement_sized(
        &mut self,
        state: &State,
        wanted: Option<Size<i32, Logical>>,
    ) -> Rectangle<i32, Logical> {
        let target = state
            .new_window_output()
            .map(|entry| Rectangle::new(entry.loc.into(), entry.size));
        self.placement_on(state, wanted, target)
    }

    fn placement_on(
        &mut self,
        state: &State,
        wanted: Option<Size<i32, Logical>>,
        target: Option<Rectangle<i32, Logical>>,
    ) -> Rectangle<i32, Logical> {
        let work = target
            .map(|output| {
                Rectangle::new(
                    (output.loc.x, output.loc.y + WORK_AREA_TOP).into(),
                    (output.size.w, (output.size.h - WORK_AREA_TOP).max(0)).into(),
                )
            })
            .unwrap_or_else(|| Self::work_area(state));
        if work.size.w <= 0 || work.size.h <= 0 {
            let mut rect = self.cascade_geometry();
            if let Some(size) = wanted {
                rect.size = size;
            }
            return rect;
        }
        let (w, h) = wanted
            .map(|s| (s.w, s.h))
            .unwrap_or((DEFAULT_WIDTH, DEFAULT_HEIGHT));
        let size: Size<i32, Logical> = (w.min(work.size.w), h.min(work.size.h)).into();
        let active = self.model.active_workspace();
        let others: Vec<Point<i32, Logical>> = self
            .stacking
            .iter()
            .filter(|id| {
                self.model
                    .window(**id)
                    .is_some_and(|m| m.workspace == active)
            })
            .filter_map(|id| self.windows.get(id))
            .filter(|w| !w.unplaced && !w.minimized && work.contains(w.geometry.loc))
            .map(|w| w.geometry.loc)
            .collect();
        let centered: Point<i32, Logical> = (
            work.loc.x + (work.size.w - size.w) / 2,
            work.loc.y + (work.size.h - size.h) / 2,
        )
            .into();
        let mut loc = if others.is_empty() {
            centered
        } else {
            next_cascade(centered, others, size, work)
        };
        loc.x = loc.x.max(work.loc.x);
        loc.y = loc.y.max(work.loc.y);
        Rectangle::new(loc, size)
    }

    /// An explicit move, resize or layout change places a window: from
    /// then on its geometry is the compositor's, not the client's pick.
    fn settle(&mut self, id: u64) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.unplaced = false;
        }
    }

    /// Place windows whose first commit has arrived at the size the
    /// client chose: GNOME's automatic placement (centred, or cascaded
    /// from the last window), dialogs centred over their parent. A
    /// window that left floating before committing keeps its layout.
    fn place_committed(&mut self, state: &State) {
        let ready: Vec<(u64, Size<i32, Logical>)> = self
            .windows
            .iter()
            .filter(|(_, w)| w.unplaced)
            .filter_map(|(id, w)| {
                let size = committed_size(w.surface.wl_surface()?.as_ref())?;
                (size.w > 0 && size.h > 0).then_some((*id, size))
            })
            .collect();
        for (id, size) in ready {
            let floating = self
                .windows
                .get(&id)
                .is_some_and(|w| w.layout == WindowLayout::Floating);
            if floating {
                // Preserve the target selected before the first buffer, even
                // if the pointer moved while the client was rendering it.
                let target = self.windows.get(&id).and_then(|window| {
                    state
                        .outputs
                        .iter()
                        .find(|entry| {
                            Rectangle::new(entry.loc.into(), entry.size)
                                .contains(window.geometry.loc)
                        })
                        .map(|entry| Rectangle::new(entry.loc.into(), entry.size))
                });
                let geometry = self.placement_on(state, Some(size), target);
                if let Some(window) = self.windows.get_mut(&id) {
                    window.geometry = geometry;
                    window.unplaced = false;
                }
                let parent = self.transient_parent(id);
                if let Some(parent) = parent {
                    self.place_above(parent, id);
                }
                let focused = self.model.focused() == Some(id);
                self.configure(id, focused);
            } else if let Some(window) = self.windows.get_mut(&id) {
                window.unplaced = false;
            }
        }
    }

    /// Next cascaded floating geometry.
    fn cascade_geometry(&mut self) -> Rectangle<i32, Logical> {
        let offset = self.cascade % 320;
        self.cascade = self.cascade.wrapping_add(CASCADE_STEP);
        Rectangle {
            loc: (offset, offset).into(),
            size: (DEFAULT_WIDTH, DEFAULT_HEIGHT).into(),
        }
    }

    /// Model insert on the active workspace. Returns the model id.
    fn insert_managed(&mut self, title: &str, app_id: Option<&str>) -> u64 {
        self.model
            .insert(title, app_id, self.model.active_workspace())
    }

    /// Register one X11 window: same model insert, cascade, configure,
    /// and focus as a native map, with identity from the X11 title and
    /// WM_CLASS. (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    fn map_x11(&mut self, state: &mut State, surface: &X11Surface) -> u64 {
        let (title, app_id) =
            map_x11_identity(&surface.title(), &surface.class(), &surface.instance());
        let window = Window::new_x11_window(surface.clone());
        let id = self.insert_managed(&title, app_id.as_deref());
        self.x11_index.insert(surface.window_id(), id);
        eprintln!("roost-compositor: X11 window {id} mapped ({title})");
        // GNOME placement at the size the client asked for.
        let asked = surface.geometry().size;
        let wanted = (asked.w > 0 && asked.h > 0).then_some(asked);
        let geometry = self.placement_sized(state, wanted);
        self.windows.insert(
            id,
            ManagedWindow {
                surface: window,
                geometry,
                layout: WindowLayout::Floating,
                restore: None,
                preset: None,
                unplaced: false,
                minimized: false,
                above: false,
                sticky: false,
                x11_parent: None,
            },
        );
        if let Some(parent) = surface
            .is_transient_for()
            .and_then(|parent| self.x11_index.get(&parent).copied())
        {
            self.place_above(parent, id);
        }
        self.finish_map(state, id);
        id
    }

    /// Remove compatibility windows after the server connection is lost.
    /// Native windows keep their model IDs and surfaces across a restart.
    #[cfg(feature = "xwayland")]
    pub fn clear_x11_windows(&mut self, state: &mut State) {
        let ids: Vec<_> = self.x11_index.values().copied().collect();
        for id in ids {
            self.unmap(state, id);
        }
        state.x11_events.clear();
    }

    /// Drain queued X11 manager events: map/unmap/size and identity
    /// updates plus maximize/fullscreen requests, all on the one
    /// manager call path. (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    fn drain_x11_events(&mut self, state: &mut State) {
        for event in state.take_x11_events() {
            match event {
                X11ManagerEvent::MapRequest(surface) => {
                    if self.x11_index.contains_key(&surface.window_id()) {
                        continue;
                    }
                    self.map_x11(state, &surface);
                }
                X11ManagerEvent::Unmapped(id) | X11ManagerEvent::Destroyed(id) => {
                    if let Some(model) = self.x11_index.get(&id).copied() {
                        self.unmap(state, model);
                    }
                }
                X11ManagerEvent::ConfigureRequest {
                    id,
                    surface,
                    width,
                    height,
                } => {
                    let Some(model) = self.x11_index.get(&id).copied() else {
                        // A toolkit can request its real size before MapRequest.
                        // Dropping it leaves the initial X geometry unchanged;
                        // map_x11 would then advertise that placeholder back.
                        let mut geometry = surface.geometry();
                        geometry.size = requested_x11_size(geometry.size, width, height);
                        if surface.configure(Some(geometry)).is_err() {
                            eprintln!("roost-compositor: pre-map X11 configure refused id={id}");
                        }
                        continue;
                    };
                    if let Some(window) = self.windows.get_mut(&model) {
                        window.geometry.size =
                            requested_x11_size(window.geometry.size, width, height);
                    }
                    self.configure(model, self.model.focused() == Some(model));
                }
                X11ManagerEvent::Property(id) => {
                    let pair = self.x11_index.get(&id).copied().and_then(|model| {
                        self.windows.get(&model).and_then(|window| {
                            window.surface.x11_surface().map(|surface| {
                                (
                                    model,
                                    map_x11_identity(
                                        &surface.title(),
                                        &surface.class(),
                                        &surface.instance(),
                                    ),
                                )
                            })
                        })
                    });
                    if let Some((model, (title, app_id))) = pair {
                        self.model.update(
                            model,
                            WindowUpdate {
                                title: Some(title),
                                app_id: Some(app_id),
                                ..Default::default()
                            },
                        );
                    }
                }
                X11ManagerEvent::Maximize(id, on) => {
                    if let Some(model) = self.x11_index.get(&id).copied() {
                        self.set_maximized(state, model, on);
                    }
                }
                X11ManagerEvent::Fullscreen(id, on) => {
                    if let Some(model) = self.x11_index.get(&id).copied() {
                        self.set_fullscreen(state, model, on);
                    }
                }
            }
        }
    }

    /// Give the seat to a window that was focused before it had a
    /// surface (an X11 window mapped before association), once the
    /// surface exists. Only that case: re-asserting the model focus on
    /// every tick would take keyboard focus back from popup grabs and
    /// panel menus. (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    fn sync_seat_focus(&mut self, state: &mut State) {
        let Some(id) = self.focus_awaits_surface else {
            return;
        };
        if self.model.focused() != Some(id) || !self.windows.contains_key(&id) {
            self.focus_awaits_surface = None;
            return;
        }
        if self.overview_open || self.overview_held.is_some() {
            return;
        }
        let ready = self
            .windows
            .get(&id)
            .and_then(|window| window.surface.wl_surface())
            .is_some();
        if ready {
            self.apply_focus(state, Some(id));
        }
    }

    /// Stack a transient directly above its parent and center it
    /// there, so dialogs open over (and with focus over) the window
    /// that spawned them. Parentless windows and orphans keep their
    /// cascaded geometry and stacking slot.
    fn place_transient(&mut self, surface: &ToplevelSurface, id: u64) {
        let _ = surface;
        let Some(parent_id) = self.transient_parent(id) else {
            return;
        };
        self.place_above(parent_id, id);
    }

    /// Stack `id` directly above its parent and center it there, so
    /// dialogs open over (and with focus over) the window that
    /// spawned them. Self-parents, parentless windows, and orphans
    /// keep their cascaded geometry and stacking slot.
    fn place_above(&mut self, parent_id: u64, id: u64) {
        if parent_id == id {
            return;
        }
        self.stacking.retain(|other| *other != id);
        let pos = self
            .stacking
            .iter()
            .position(|other| *other == parent_id)
            .map(|i| i + 1)
            .unwrap_or(self.stacking.len());
        self.stacking.insert(pos.min(self.stacking.len()), id);
        if let Some(parent_geo) = self.geometry(parent_id) {
            if let Some(window) = self.windows.get_mut(&id) {
                let size = window.geometry.size;
                let x = parent_geo.loc.x + (parent_geo.size.w - size.w) / 2;
                let y = parent_geo.loc.y + (parent_geo.size.h - size.h) / 2;
                window.geometry.loc = (x, y).into();
            }
        }
    }

    /// Remove one window from the model and the index. Focus falls back
    /// to the topmost remaining window, if any. In scroll mode the
    /// remaining columns close ranks behind it.
    fn unmap(&mut self, state: &mut State, id: u64) {
        let return_parent = self
            .windows
            .get(&id)
            .and_then(|w| {
                w.x11_parent.or_else(|| {
                    let surface = w.surface.wl_surface()?;
                    let parent = state.closed_toplevel_parents.get(surface.as_ref())?;
                    self.surface_index.get(parent).copied()
                })
            })
            .filter(|parent| {
                self.model.focused() == Some(id)
                    && self
                        .windows
                        .get(parent)
                        .is_some_and(|window| window.surface.alive())
            });
        if let Some(window) = self.windows.remove(&id) {
            eprintln!("roost-compositor: window {id} unmapped");
            match window.surface.underlying_surface() {
                WindowSurface::Wayland(toplevel) => {
                    self.surface_index.remove(toplevel.wl_surface());
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(surface) => {
                    self.x11_index.remove(&surface.window_id());
                    state.window_icons.remove_cache(&format!("x11:{id}"));
                }
            }
        }
        self.stacking.retain(|other| *other != id);
        self.model.remove(id);
        for window in self.windows.values_mut() {
            if window.x11_parent == Some(id) {
                window.x11_parent = None;
            }
        }
        let fallback = return_parent.or_else(|| self.stacking.last().copied());
        self.apply_focus(state, fallback);
        if self.mode == SessionMode::Scroll {
            self.relayout_strip(state);
        }
    }

    /// Apply queued client window-state requests (maximize, unmaximize,
    /// fullscreen, unfullscreen). Unknown or already-gone surfaces are
    /// dropped: a client that asked and vanished needs nothing.
    fn drain_window_requests(&mut self, state: &mut State) {
        for (wl, request) in state.take_window_requests() {
            let Some(id) = self.surface_index.get(&wl).copied() else {
                continue;
            };
            match request {
                WindowRequest::Maximize => {
                    self.set_maximized(state, id, true);
                }
                WindowRequest::Unmaximize => {
                    self.set_maximized(state, id, false);
                }
                WindowRequest::Fullscreen => {
                    self.set_fullscreen(state, id, true);
                }
                WindowRequest::Unfullscreen => {
                    self.set_fullscreen(state, id, false);
                }
                WindowRequest::Move(serial, origin) => {
                    if self.grab.is_none()
                        && state.seat.get_pointer().is_some_and(|p| p.has_grab(serial))
                    {
                        self.begin_move_from(state, id, origin);
                        self.grab_motion(self.pointer_pos);
                    }
                }
                WindowRequest::Resize(edges, serial, origin) => {
                    if self.grab.is_none()
                        && state.seat.get_pointer().is_some_and(|p| p.has_grab(serial))
                    {
                        self.begin_resize_from(id, edges, origin);
                        self.grab_motion(self.pointer_pos);
                    }
                }
                WindowRequest::Activate => {
                    self.focus(state, Some(id));
                }
                WindowRequest::Minimize => {
                    self.minimize(state, id);
                }
                WindowRequest::Menu(x, y) => {
                    // Relative to the surface (Mutter adds it to the
                    // buffer origin), not the window geometry: GTK's
                    // shadow margin is part of it.
                    if let Some(window) = self.windows.get(&id) {
                        let loc = crate::popup::surface_origin(&wl, window.geometry.loc);
                        self.menu_requests.push((id, loc.x + x, loc.y + y));
                    }
                }
            }
        }
    }

    /// Copy the client's current title into the model entry.
    fn sync_title(&mut self, id: u64, surface: &ToplevelSurface) {
        let title = read_title(surface);
        let app_id = Some(read_app_id(surface));
        if title.is_some() || app_id != Some(None) {
            self.model.update(
                id,
                WindowUpdate {
                    title,
                    app_id,
                    ..Default::default()
                },
            );
        }
    }

    /// Focus a window: model focus, keyboard focus, pointer-adjacent
    /// activation flags, and configure round-trips. `None` unfocuses.
    /// The window rises to the top of the stacking order.
    pub fn focus(&mut self, state: &mut State, id: Option<u64>) -> bool {
        if let Some(id) = id {
            if !self.windows.contains_key(&id) {
                return false;
            }
        }
        // While the overview holds keyboard focus, an explicit choice
        // (a preview pick, a dash or search activation) becomes the
        // window restored when it closes, instead of losing to the
        // window that was focused before it opened.
        if self.overview_held.is_some() {
            self.focus_parked(id);
            return true;
        }
        self.apply_focus(state, id);
        true
    }

    /// The window focus lands on when `id` is asked for: its mapped
    /// modal dialog (that dialog's own, in turn), else `id` itself.
    fn modal_target(&self, mut id: u64) -> u64 {
        // Bounded: a parent cycle a client builds cannot spin here.
        for _ in 0..8 {
            if !self.windows.contains_key(&id) {
                break;
            }
            let dialog = self.stacking.iter().rev().copied().find(|child| {
                if *child == id {
                    return false;
                }
                let Some(w) = self.windows.get(child) else {
                    return false;
                };
                let modal = w.x11_parent.is_some()
                    || match w.surface.underlying_surface() {
                        WindowSurface::Wayland(toplevel) => read_modal(toplevel),
                        #[cfg(feature = "xwayland")]
                        WindowSurface::X11(_) => false,
                    };
                !w.minimized && modal && self.transient_parent(*child) == Some(id)
            });
            match dialog {
                Some(child) => id = child,
                None => break,
            }
        }
        id
    }

    /// Focus while the overview holds the keyboard: the window is the
    /// one restored on close, rises, and takes the activated look.
    fn focus_parked(&mut self, id: Option<u64>) {
        self.pre_overview_focus = id;
        if let Some(id) = id {
            if self.mode == SessionMode::Gnome {
                self.stacking.retain(|other| *other != id);
                self.stacking.push(id);
            }
        }
        if self.overview_activated != id {
            if let Some(old) = self.overview_activated {
                self.configure(old, false);
            }
            if let Some(id) = id {
                self.configure(id, true);
            }
            self.overview_activated = id;
            self.activated = id;
        }
    }

    fn apply_focus(&mut self, state: &mut State, id: Option<u64>) {
        // GNOME attaches modal dialogs to their parent: the parent
        // cannot take focus from its dialog (xdg-dialog, #89).
        let id = id.map(|id| self.modal_target(id));
        let previous = self.model.focused();
        self.model.set_focused(id);
        // Shell activation may set model focus before this manager runs.
        // Compare the last applied activation too, so Alt+Tab follows the
        // selected column while ordinary reassertion preserves wheel panning.
        if self.mode == SessionMode::Scroll
            && id.is_some()
            && (previous != id || self.activated != id)
        {
            self.follow_focus(state);
            self.relayout_strip(state);
        }
        if let Some(id) = id {
            // Activating a hidden window brings it back (GNOME).
            if let Some(window) = self.windows.get_mut(&id) {
                window.minimized = false;
            }
            // Floating stacks raise focus to the top; the strip keeps
            // column order independent of focus (niri shape), so focus
            // never reorders in scroll mode.
            if self.mode == SessionMode::Gnome {
                self.stacking.retain(|other| *other != id);
                self.stacking.push(id);
                self.keep_above_on_top();
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        for old in [previous, self.activated.take()].into_iter().flatten() {
            if Some(old) != id {
                self.configure(old, false);
            }
        }
        if let Some(id) = id {
            self.configure(id, true);
            self.activated = Some(id);
            if let (Some(keyboard), Some(window)) = (self.keyboard.clone(), self.windows.get(&id)) {
                // Unassociated X11 windows contribute no surface yet;
                // the model focus stands and the seat follows on
                // association via the reconcile re-sync.
                let focus = window
                    .surface
                    .wl_surface()
                    .map(|surface| surface.into_owned());
                self.focus_awaits_surface = focus.is_none().then_some(id);
                keyboard.set_focus(state, focus, serial);
            }
        } else if let Some(keyboard) = self.keyboard.clone() {
            keyboard.set_focus(state, None, serial);
        }
        // Clipboard offers follow keyboard focus, with or without a
        // keyboard capability attached.
        let surface = id.and_then(|id| {
            self.windows.get(&id).and_then(|window| {
                window
                    .surface
                    .wl_surface()
                    .map(|surface| surface.into_owned())
            })
        });
        state.sync_selection_focus(surface.as_ref());
    }

    /// Send a configure advertising this window's geometry, activation
    /// flag, and presentation state. X11 windows take the toolkit's
    /// X11 configure path with the same geometry and activation.
    fn configure(&self, id: u64, activated: bool) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let layout = window.layout;
        match window.surface.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                // An unplaced floating window gets no size: the client
                // picks its own.
                let size = (!(window.unplaced && layout == WindowLayout::Floating))
                    .then_some(window.geometry.size);
                Self::configure_wayland(toplevel, &layout, size, activated);
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(surface) => {
                if surface.set_activated(activated).is_err()
                    || surface.configure(Some(window.geometry)).is_err()
                {
                    eprintln!("roost-compositor: window {id} X11 configure refused");
                }
            }
        }
    }

    /// Advertise geometry, activation, and presentation state to one
    /// native client.
    fn configure_wayland(
        surface: &ToplevelSurface,
        layout: &WindowLayout,
        size: Option<Size<i32, Logical>>,
        activated: bool,
    ) {
        surface.with_pending_state(|pending| {
            pending.size = size;
            if activated {
                pending.states.set(xdg_toplevel::State::Activated);
            } else {
                pending.states.unset(xdg_toplevel::State::Activated);
            }
            // Exactly one presentation state: managed layouts advertise
            // theirs, floating advertises none.
            for state in [
                xdg_toplevel::State::Maximized,
                xdg_toplevel::State::Fullscreen,
                xdg_toplevel::State::TiledLeft,
                xdg_toplevel::State::TiledRight,
                xdg_toplevel::State::TiledTop,
                xdg_toplevel::State::TiledBottom,
            ] {
                pending.states.unset(state);
            }
            match layout {
                WindowLayout::Floating => {}
                // niri tells its columns they are tiled on every edge:
                // client-side decorations drop their shadow and rounded
                // corners, so columns meet the gaps (and the focus ring)
                // square.
                WindowLayout::Strip => {
                    for state in [
                        xdg_toplevel::State::TiledLeft,
                        xdg_toplevel::State::TiledRight,
                        xdg_toplevel::State::TiledTop,
                        xdg_toplevel::State::TiledBottom,
                    ] {
                        pending.states.set(state);
                    }
                }
                WindowLayout::Maximized => {
                    pending.states.set(xdg_toplevel::State::Maximized);
                }
                WindowLayout::Fullscreen => {
                    pending.states.set(xdg_toplevel::State::Fullscreen);
                }
                WindowLayout::Tiled(TileSide::Left) => {
                    pending.states.set(xdg_toplevel::State::TiledLeft);
                }
                WindowLayout::Tiled(TileSide::Right) => {
                    pending.states.set(xdg_toplevel::State::TiledRight);
                }
            }
        });
        surface.send_configure();
    }

    /// Start moving `id` with the pointer (header-bar drag or Super+drag).
    /// A maximized or tiled window drags off into floating first, keeping
    /// the pointer at the same fraction across its width (GNOME shape).
    fn begin_move(&mut self, state: &mut State, id: u64) {
        self.begin_move_from(state, id, self.pointer_pos);
    }

    fn begin_move_from(&mut self, state: &mut State, id: u64, pointer: Point<f64, Logical>) {
        self.settle(id);
        if self.mode == SessionMode::Scroll {
            return;
        }
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let managed = window.layout != WindowLayout::Floating;
        let before = window.geometry;
        if managed {
            self.restore_layout(id);
            if let Some(window) = self.windows.get_mut(&id) {
                let frac = if before.size.w > 0 {
                    (pointer.x - f64::from(before.loc.x)) / f64::from(before.size.w)
                } else {
                    0.5
                };
                let w = window.geometry.size.w;
                window.geometry.loc = (
                    (pointer.x - frac * f64::from(w)).round() as i32,
                    (pointer.y - 16.0).round() as i32,
                )
                    .into();
            }
            let focused = self.model.focused() == Some(id);
            self.configure(id, focused);
        }
        if let Some(window) = self.windows.get(&id) {
            self.grab = Some(PointerGrab::Move {
                id,
                pointer_start: pointer,
                loc_start: window.geometry.loc,
            });
        }
        let _ = state;
    }

    /// Start resizing `id` from `edges` with the pointer.
    fn begin_resize(&mut self, id: u64, edges: u32) {
        self.begin_resize_from(id, edges, self.pointer_pos);
    }

    fn begin_resize_from(&mut self, id: u64, edges: u32, pointer: Point<f64, Logical>) {
        self.settle(id);
        if self.mode == SessionMode::Scroll {
            return;
        }
        if !self.restore_layout(id) {
            return;
        }
        if let Some(window) = self.windows.get(&id) {
            self.grab = Some(PointerGrab::Resize {
                id,
                edges,
                pointer_start: pointer,
                geometry_start: window.geometry,
            });
        }
    }

    /// Whether an interactive move/resize is in progress.
    pub fn grab_active(&self) -> bool {
        self.grab.is_some()
    }

    /// Follow the pointer during a grab. Returns whether a grab ate it.
    fn grab_motion(&mut self, pos: Point<f64, Logical>) -> bool {
        let Some(grab) = self.grab else {
            return false;
        };
        match grab {
            PointerGrab::Move {
                id,
                pointer_start,
                loc_start,
            } => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.geometry.loc = (
                        loc_start.x + (pos.x - pointer_start.x).round() as i32,
                        loc_start.y + (pos.y - pointer_start.y).round() as i32,
                    )
                        .into();
                }
            }
            PointerGrab::Resize {
                id,
                edges,
                pointer_start,
                geometry_start,
            } => {
                let dx = (pos.x - pointer_start.x).round() as i32;
                let dy = (pos.y - pointer_start.y).round() as i32;
                let next = resized(geometry_start, edges, dx, dy);
                let changed = self.windows.get(&id).is_some_and(|w| w.geometry != next);
                if changed {
                    if let Some(window) = self.windows.get_mut(&id) {
                        window.geometry = next;
                    }
                    let focused = self.model.focused() == Some(id);
                    self.configure(id, focused);
                }
            }
        }
        true
    }

    /// End the grab on button release. A move dropped at the top edge
    /// maximizes; at a side edge it tiles that half (GNOME snapping).
    fn end_grab(&mut self, state: &mut State) {
        let Some(grab) = self.grab.take() else {
            return;
        };
        if let PointerGrab::Move { id, .. } = grab {
            match snap_target(self.pointer_pos, state.primary_size().w) {
                Some(Snap::Maximize) => {
                    self.set_maximized(state, id, true);
                }
                Some(Snap::Tile(side)) => {
                    self.set_tiled(state, id, side);
                }
                None => {}
            }
        }
    }

    /// GNOME's tile preview while a moved window is over a snap edge:
    /// the dragged window and the area it would fill on release (the
    /// work area, or one half of it).
    pub fn tile_preview(&self, state: &State) -> Option<(u64, Rectangle<i32, Logical>)> {
        let Some(PointerGrab::Move { id, .. }) = self.grab else {
            return None;
        };
        let rect = match snap_target(self.pointer_pos, state.primary_size().w)? {
            Snap::Maximize => Self::work_area(state),
            Snap::Tile(side) => Self::tile_area(state, side),
        };
        Some((id, rect))
    }

    /// Every visible popup placed in the global space, bottom to top:
    /// window popups in stacking order, then layer-surface popups (the
    /// layers they hang off paint above windows).
    pub fn placed_popups(&self, state: &State) -> Vec<crate::popup::PlacedPopup> {
        let mut out = Vec::new();
        for (window, geometry) in self.visible_windows() {
            if let Some(surface) = window.wl_surface() {
                let origin = crate::popup::surface_origin(&surface, geometry.loc);
                out.extend(crate::popup::placed_popups(&surface, origin, true));
            }
        }
        for (surface, (x, y), _) in crate::layer::layer_layout(state) {
            out.extend(crate::popup::placed_popups(&surface, (x, y).into(), false));
        }
        out
    }

    /// Topmost popup containing `pos`, if any (#88).
    pub fn popup_at(
        &self,
        state: &State,
        pos: Point<f64, Logical>,
    ) -> Option<crate::popup::PlacedPopup> {
        let placed = self.placed_popups(state);
        crate::popup::popup_at(&placed, pos).cloned()
    }

    /// The surface a press at `pos` would land on (layer or window).
    fn surface_for_click(&self, state: &State, pos: Point<f64, Logical>) -> Option<WlSurface> {
        if let Some((surface, _)) =
            crate::layer::topmost_layer_at(state, pos.x.floor() as i32, pos.y.floor() as i32)
        {
            return Some(surface);
        }
        let id = self.window_at(pos)?;
        self.windows
            .get(&id)
            .and_then(|w| w.surface.wl_surface())
            .map(|s| s.into_owned())
    }

    /// Topmost window containing `pos`, if any. Only the active
    /// workspace is hit-testable; hidden workspaces never take focus.
    pub fn window_at(&self, pos: Point<f64, Logical>) -> Option<u64> {
        let active = self.model.active_workspace();
        self.stacking.iter().rev().copied().find(|id| {
            self.model.window(*id).is_some_and(|entry| {
                entry.workspace == active
                    && self
                        .windows
                        .get(id)
                        .is_some_and(|w| contains(w.geometry, pos))
            })
        })
    }

    /// Pointer motion: track the position, focus the window under the
    /// cursor, and deliver the motion event to it. Focusing on motion
    /// (not only on click) keeps pointer focus off the shell round-trip.
    /// Layer-shell surfaces (panel, banners) paint over windows, so they
    /// win the hit test first; without this the panel never sees motion.
    pub fn pointer_motion(&mut self, state: &mut State, pos: Point<f64, Logical>, time: u32) {
        // A locked pointer stays where it is (relative motion still
        // flows); a confined one stays inside its window.
        let Some(pos) = self.constrain(state, pos) else {
            return;
        };
        self.pointer_pos = pos;
        // An interactive move/resize owns the pointer until release.
        if self.grab_motion(pos) {
            return;
        }
        // Popups paint over everything they hang off: they win first.
        if let Some(popup) = self.popup_at(state, pos) {
            if let Some(pointer) = self.pointer.clone() {
                pointer.motion(
                    state,
                    Some((popup.surface, popup.origin.to_f64())),
                    &MotionEvent {
                        location: pos,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                // wl_seat v5+: clients act on pointer events only at a
                // frame boundary (GTK4 drops unframed clicks).
                pointer.frame(state);
            }
            return;
        }
        if let Some((surface, (ox, oy))) =
            crate::layer::topmost_layer_at(state, pos.x.floor() as i32, pos.y.floor() as i32)
        {
            if let Some(pointer) = self.pointer.clone() {
                // Focus point is the surface origin: smithay reports
                // surface-local coordinates as event minus focus.
                pointer.motion(
                    state,
                    Some((surface, (ox as f64, oy as f64).into())),
                    &MotionEvent {
                        location: pos,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                // wl_seat v5+: clients act on pointer events only at a
                // frame boundary (GTK4 drops unframed clicks).
                pointer.frame(state);
            }
            return;
        }
        // In the overview, windows are previews the compositor draws:
        // they never take pointer focus or events (GNOME shape).
        if self.overview_open {
            if let Some(pointer) = self.pointer.clone() {
                pointer.motion(
                    state,
                    None,
                    &MotionEvent {
                        location: pos,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                pointer.frame(state);
            }
            return;
        }
        // Pointer focus is the window under the pointer, or nothing over
        // the bare desktop. Keyboard focus and stacking change only on a
        // click (GNOME's default click-to-focus), never on motion.
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        let target = self
            .window_at(pos)
            // An attached modal dialog blocks pointer input to its parent,
            // even where the parent's content is exposed beside the dialog.
            .filter(|id| self.modal_target(*id) == *id)
            .and_then(|id| self.windows.get(&id))
            .and_then(|window| {
                let surface = window.surface.wl_surface()?.into_owned();
                // Focus point is the surface origin: smithay reports
                // surface-local coordinates as event minus focus. The
                // window rect is its xdg geometry; the surface origin
                // sits up-left of it by the client-side shadow.
                let origin = crate::popup::surface_origin(&surface, window.geometry.loc);
                Some((surface, (origin.x as f64, origin.y as f64).into()))
            });
        pointer.motion(
            state,
            target,
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        // wl_seat v5+: clients act on pointer events only at a frame
        // boundary (GTK4 drops unframed clicks).
        pointer.frame(state);
    }

    /// Pointer button: deliver to the focused window and focus the
    /// window under the cursor on press (click-to-focus). A press on a
    /// layer-shell surface moves keyboard focus there (unless the
    /// overview park owns it, or the surface asked for no keyboard, as
    /// IBus's candidate window does) so panel menus take keys; the
    /// button itself follows pointer focus from the last motion.
    pub fn pointer_button(&mut self, state: &mut State, button: u32, pressed: bool, time: u32) {
        let trace = std::env::var_os("ROOST_POINTER_TRACE").is_some();
        if trace {
            eprintln!(
                "roost-compositor: pointer trace: manager button received popup_grab={} move_modifier={}",
                state.popup_grab_active(), self.super_held
            );
        }
        // Release ends a move/resize grab. It is still delivered (unless
        // its press was swallowed): the client's press opened smithay's
        // implicit click grab, and only the matching release closes it.
        // Dropping it pinned pointer focus to the dragged window for
        // good, so the panel never saw another click (#97).
        if !pressed && self.grab.is_some() {
            self.end_grab(state);
        }
        // A blocked parent click activates its modal dialog and is consumed.
        // Do this before Super+drag so the parent cannot be moved through it.
        if pressed && !self.overview_open {
            let pos = self.pointer_pos;
            if crate::layer::topmost_layer_at(state, pos.x.floor() as i32, pos.y.floor() as i32)
                .is_none()
                && self.popup_at(state, pos).is_none()
            {
                if let Some(id) = self.window_at(pos) {
                    if self.modal_target(id) != id {
                        self.apply_focus(state, Some(id));
                        self.swallowed_button = Some(button);
                        return;
                    }
                }
            }
        }
        // Super+press on a window starts a move (GNOME's Super+drag).
        if pressed && self.super_held && !self.overview_open {
            let pos = self.pointer_pos;
            if crate::layer::topmost_layer_at(state, pos.x.floor() as i32, pos.y.floor() as i32)
                .is_none()
                && self.popup_at(state, pos).is_none()
            {
                if let Some(id) = self.window_at(pos) {
                    self.apply_focus(state, Some(id));
                    // The trigger machine already disarmed the Super tap on
                    // this press, so releasing Super will not open the
                    // overview.
                    self.begin_move(state, id);
                    // The press is not delivered, so neither is its release.
                    self.swallowed_button = Some(button);
                    return;
                }
            }
        }
        let on_popup = self.popup_at(state, self.pointer_pos).is_some();
        // xdg-shell grab rule: a press on another client's surface (or on
        // nothing) dismisses the grabbed popups and is consumed, so the
        // click never reaches what is beneath. A press on a surface of
        // the grab's own client is delivered normally: that client
        // decides (a GTK panel closes one popover and opens the next).
        if pressed && !on_popup && state.popup_grab_active() {
            let target = self
                .surface_for_click(state, self.pointer_pos)
                .and_then(|s| s.client())
                .map(|c| c.id());
            if target.is_none() || target != state.popup_grab_owner() {
                state.dismiss_popup_grab();
                self.swallowed_button = Some(button);
                return;
            }
        }
        if !pressed && self.swallowed_button == Some(button) {
            self.swallowed_button = None;
            return;
        }
        if pressed && !on_popup {
            let pos = self.pointer_pos;
            if let Some((surface, _)) =
                crate::layer::topmost_layer_at(state, pos.x.floor() as i32, pos.y.floor() as i32)
            {
                if !self.overview_open && crate::layer::surface_takes_keyboard_on_press(&surface) {
                    let serial = SERIAL_COUNTER.next_serial();
                    if let Some(keyboard) = self.keyboard.clone() {
                        keyboard.set_focus(state, Some(surface.clone()), serial);
                    }
                    state.sync_selection_focus(Some(&surface));
                }
            } else if self.overview_open {
                // Overview presses are the runtime's (preview hits).
            } else if let Some(id) = self.window_at(pos) {
                // A lock or on-demand layer can own the keyboard while the
                // model still names this window. A click must restore actual
                // seat focus even when the model's focused ID is unchanged.
                let keyboard_matches = self
                    .keyboard
                    .as_ref()
                    .is_none_or(|keyboard| keyboard.current_focus() == self.surface_of(id));
                if self.model.focused() != Some(id) || !keyboard_matches {
                    self.apply_focus(state, Some(id));
                }
            }
        }
        // Mapping, closing or restacking a surface can change what lies
        // under a stationary pointer. Refresh enter/focus before the press;
        // releases still follow Smithay's implicit grab to their original owner.
        if pressed {
            self.pointer_motion(state, self.pointer_pos, time);
        }
        if let Some(pointer) = self.pointer.clone() {
            if trace {
                eprintln!(
                    "roost-compositor: pointer trace: client button dispatched focused={}",
                    pointer.current_focus().is_some()
                );
            }
            pointer.button(
                state,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                    button,
                    state: if pressed {
                        ButtonState::Pressed
                    } else {
                        ButtonState::Released
                    },
                },
            );
            // wl_seat v5+: clients act on pointer events only at a
            // frame boundary (GTK4 drops unframed clicks).
            pointer.frame(state);
        }
    }

    /// Scroll the client under the pointer. Wheel notches arrive in v120
    /// units (multiples of 120): sent as discrete steps of 15 px each,
    /// Mutter's wheel distance. Anything else is a continuous (touchpad)
    /// distance in pixels. Always closed by a pointer frame.
    pub fn pointer_axis(&mut self, state: &mut State, horizontal: f64, vertical: f64, time: u32) {
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        let mut frame = AxisFrame::new(time);
        let mut wheel = false;
        let mut any = false;
        for (axis, amount) in [(Axis::Horizontal, horizontal), (Axis::Vertical, vertical)] {
            if amount == 0.0 {
                continue;
            }
            any = true;
            if is_v120_wheel(amount) {
                wheel = true;
                frame = frame
                    .value(axis, amount / 120.0 * WHEEL_STEP_PX)
                    .v120(axis, amount as i32);
            } else {
                frame = frame.value(axis, amount);
            }
        }
        if !any {
            return;
        }
        frame = frame.source(if wheel {
            smithay::backend::input::AxisSource::Wheel
        } else {
            smithay::backend::input::AxisSource::Finger
        });
        pointer.axis(state, frame);
        pointer.frame(state);
    }

    /// Route one input event to the session-lock surface and nothing
    /// else (ext-session-lock-v1): keys go to it with keyboard focus
    /// held there, the pointer acts on it at the output origin, and no
    /// compositor shortcut fires. Gestures and relative motion are
    /// dropped: nothing behind the lock may hear them.
    pub fn lock_input(
        &mut self,
        state: &mut State,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        input: ManagerInput,
    ) {
        self.note_modifiers(&input);
        if let Some(keyboard) = self.keyboard.clone() {
            if keyboard.current_focus().as_ref() != Some(surface) {
                keyboard.set_focus(state, Some(surface.clone()), SERIAL_COUNTER.next_serial());
            }
        }
        match input {
            ManagerInput::Key {
                keycode,
                pressed,
                time,
            } => {
                self.lock_input_active = true;
                self.keyboard_key(state, keycode, pressed, time);
                self.lock_input_active = false;
            }
            ManagerInput::Motion { pos, time } => {
                self.pointer_pos = pos;
                if let Some(pointer) = self.pointer.clone() {
                    pointer.motion(
                        state,
                        Some((surface.clone(), (0.0, 0.0).into())),
                        &MotionEvent {
                            location: pos,
                            serial: SERIAL_COUNTER.next_serial(),
                            time,
                        },
                    );
                    pointer.frame(state);
                }
            }
            ManagerInput::Button {
                button,
                pressed,
                time,
            } => {
                if let Some(pointer) = self.pointer.clone() {
                    pointer.button(
                        state,
                        &ButtonEvent {
                            serial: SERIAL_COUNTER.next_serial(),
                            time,
                            button,
                            state: if pressed {
                                ButtonState::Pressed
                            } else {
                                ButtonState::Released
                            },
                        },
                    );
                    pointer.frame(state);
                }
            }
            ManagerInput::Axis {
                horizontal,
                vertical,
                time,
            } => self.pointer_axis(state, horizontal, vertical, time),
            _ => {}
        }
    }

    /// Deliver a key event to the focused window. Returns false when no
    /// keyboard capability exists.
    pub fn keyboard_key(
        &mut self,
        state: &mut State,
        keycode: u32,
        pressed: bool,
        time: u32,
    ) -> bool {
        let Some(keyboard) = self.keyboard.clone() else {
            return false;
        };
        // A grabbed accelerator's release follows its press: swallowed.
        if !pressed {
            if let Some(i) = self.accel_held.iter().position(|k| *k == keycode) {
                self.accel_held.remove(i);
                keyboard.input_discard(
                    state,
                    keycode.saturating_add(XKB_X11_OFFSET).into(),
                    KeyState::Released,
                );
                return true;
            }
        }
        let mode = if self.lock_input_active {
            roost_shell_control::MODE_LOCK_SCREEN
        } else if crate::layer::exclusive_popup_keyboard_layer(state).is_some() {
            roost_shell_control::MODE_POPUP
        } else if self.overview_open {
            roost_shell_control::MODE_OVERVIEW
        } else {
            roost_shell_control::MODE_NORMAL
        };
        let mode_mask = if self.lock_input_active {
            roost_shell_control::MODE_LOCK_SCREEN | roost_shell_control::MODE_UNLOCK_SCREEN
        } else {
            mode
        };
        // A window with approved shortcut inhibition receives the shell's
        // grabbed accelerators too. Lock input always remains compositor-owned.
        let inhibited = !self.lock_input_active && state.shortcuts_inhibited();
        let grabs: Vec<roost_shell_control::Accelerator> = if pressed && !inhibited {
            self.accelerators
                .iter()
                .filter(|a| a.modes & mode_mask != 0)
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        // Smithay's keyboard input takes XKB codespace (evdev + 8):
        // it feeds the code straight to xkbcommon and sends
        // `raw - 8` on the wire, so passing our evdev tables through
        // unchanged would mistranslate every key and panic on codes
        // below 8 (Escape, digits) once a client holds keyboard focus.
        let intercepted = keyboard.input::<u32, _>(
            state,
            keycode.saturating_add(XKB_X11_OFFSET).into(),
            if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            SERIAL_COUNTER.next_serial(),
            time,
            |_, modifiers, handle| {
                if grabs.is_empty() {
                    return FilterResult::Forward;
                }
                let mods = accelerator_mods(modifiers);
                let syms: Vec<u32> = handle
                    .raw_syms()
                    .iter()
                    .map(|k| k.raw())
                    .chain(std::iter::once(handle.modified_sym().raw()))
                    .collect();
                match grabs
                    .iter()
                    .find(|a| a.mods == mods && syms.contains(&a.keysym))
                {
                    Some(a) => FilterResult::Intercept(a.action),
                    None => FilterResult::Forward,
                }
            },
        );
        if let Some(action) = intercepted {
            self.accel_held.push(keycode);
            self.accel_fired.push((action, time, mode));
        }
        true
    }

    /// GNOME's Hide: take the window off the screen and hand focus to
    /// the next window down. Returns whether it was hidden.
    pub fn minimize(&mut self, state: &mut State, id: u64) -> bool {
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        if window.minimized {
            return true;
        }
        window.minimized = true;
        if self.model.focused() == Some(id) {
            let workspace = self.model.active_workspace();
            self.focus_topmost(state, workspace);
        }
        true
    }

    /// Always on Top windows stay above every other window: a stable
    /// partition of the stacking order.
    fn keep_above_on_top(&mut self) {
        let windows = &self.windows;
        let (above, rest): (Vec<u64>, Vec<u64>) = self
            .stacking
            .iter()
            .partition(|id| windows.get(id).is_some_and(|w| w.above));
        self.stacking = rest.into_iter().chain(above).collect();
    }

    /// Whether `id` is on every workspace.
    pub fn is_sticky(&self, id: u64) -> bool {
        self.windows.get(&id).is_some_and(|w| w.sticky)
    }

    /// Whether `id` is Always on Top.
    pub fn is_above(&self, id: u64) -> bool {
        self.windows.get(&id).is_some_and(|w| w.above)
    }

    /// Whether `id` is hidden (minimized).
    pub fn is_minimized(&self, id: u64) -> bool {
        self.windows.get(&id).is_some_and(|w| w.minimized)
    }

    /// Whether `id` is maximized.
    pub fn is_maximized(&self, id: u64) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|w| w.layout == WindowLayout::Maximized)
    }

    /// Carry out one of GNOME's window-menu actions on `id`.
    pub fn window_action(
        &mut self,
        state: &mut State,
        id: u64,
        action: roost_shell_control::WindowAction,
    ) -> bool {
        use roost_shell_control::WindowAction;
        if !self.windows.contains_key(&id) {
            return false;
        }
        match action {
            WindowAction::Minimize => self.minimize(state, id),
            WindowAction::ToggleMaximize => {
                let maximized = self.is_maximized(id);
                self.set_maximized(state, id, !maximized)
            }
            WindowAction::Move => {
                self.focus(state, Some(id));
                self.begin_move(state, id);
                true
            }
            WindowAction::Resize => {
                self.focus(state, Some(id));
                // xdg_toplevel resize edge bottom_right.
                self.begin_resize(id, 10);
                true
            }
            WindowAction::ShowMenu => {
                // Mutter opens the keyboard window menu at the frame's
                // corner.
                let Some(loc) = self.windows.get(&id).map(|w| w.geometry.loc) else {
                    return false;
                };
                self.menu_requests.push((id, loc.x, loc.y));
                true
            }
            WindowAction::Unmaximize => self.set_maximized(state, id, false),
            WindowAction::Maximize => self.set_maximized(state, id, true),
            WindowAction::ToggleTiledLeft => self.toggle_tiled(state, id, TileSide::Left),
            WindowAction::ToggleTiledRight => self.toggle_tiled(state, id, TileSide::Right),
            WindowAction::MoveToWorkspace { workspace } => {
                self.move_to_workspace(state, id, workspace)
            }
            WindowAction::ToggleSticky => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.sticky = !window.sticky;
                }
                true
            }
            WindowAction::ToggleAbove => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.above = !window.above;
                }
                self.keep_above_on_top();
                true
            }
            WindowAction::MoveToWorkspaceLeft | WindowAction::MoveToWorkspaceRight => {
                let Some(current) = self.model.window(id).map(|e| e.workspace) else {
                    return false;
                };
                let target = if action == WindowAction::MoveToWorkspaceLeft {
                    match current.checked_sub(1) {
                        Some(t) => t,
                        None => return false,
                    }
                } else {
                    current.saturating_add(1)
                };
                // Mutter moves the window and leaves the view where it is.
                self.move_to_workspace(state, id, target)
            }
        }
    }

    /// Whether the window has a workspace to its left (and always one
    /// to its right: GNOME's workspaces are dynamic).
    pub fn workspace_left_of(&self, id: u64) -> bool {
        self.model.window(id).is_some_and(|e| e.workspace > 0)
    }

    /// GNOME's popup for the active workspace: `(index, count)`.
    pub fn workspace_popup(&self) -> (u32, u32) {
        let active = self.model.active_workspace();
        let occupied = self.model.windows().map(|w| w.workspace).max();
        (
            active,
            roost_shell_control::dynamic_workspace_count(occupied, active),
        )
    }

    /// Workspace-switcher popups since the last call.
    pub fn take_workspace_popups(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.workspace_popups)
    }

    /// Window-menu requests since the last call: `(window, x, y)`.
    pub fn take_menu_requests(&mut self) -> Vec<(u64, i32, i32)> {
        std::mem::take(&mut self.menu_requests)
    }

    /// Replace the accelerator grabs (org.gnome.Shell GrabAccelerators).
    pub fn set_accelerators(&mut self, accelerators: Vec<roost_shell_control::Accelerator>) {
        self.accelerators = accelerators;
    }

    /// Replace the switcher's chords (the shell's GNOME keybindings).
    pub fn set_switcher_keys(&mut self, keys: Vec<roost_shell_control::SwitcherKey>) {
        self.switcher_keys = Some(keys);
    }

    /// Accelerators fired since the last call: `(action, time, mode)`.
    pub fn take_accelerators_fired(&mut self) -> Vec<(u32, u32, u32)> {
        std::mem::take(&mut self.accel_fired)
    }

    /// Move a window by a delta, keeping it on its workspace. A manual
    /// move from a managed layout restores the stashed geometry first
    /// (GNOME drag-off shape), then applies the delta.
    pub fn move_window(&mut self, id: u64, dx: i32, dy: i32) -> bool {
        self.settle(id);
        if !self.restore_layout(id) {
            return false;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        let loc = window.geometry.loc;
        window.geometry.loc = (loc.x + dx, loc.y + dy).into();
        true
    }

    /// Resize a window and advertise the new size via configure. A
    /// manual resize from a managed layout restores the stashed
    /// geometry first, then applies the new size.
    pub fn resize_window(&mut self, id: u64, width: i32, height: i32) -> bool {
        self.settle(id);
        if width <= 0 || height <= 0 {
            return false;
        }
        if !self.restore_layout(id) {
            return false;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        window.geometry.size = (width, height).into();
        self.configure(id, self.model.focused() == Some(id));
        true
    }

    /// Return a window to floating: pop the stashed geometry when one
    /// was kept, otherwise keep the current geometry. Returns false
    /// for unknown ids.
    fn restore_layout(&mut self, id: u64) -> bool {
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        if window.layout != WindowLayout::Floating {
            if let Some(restore) = window.restore.take() {
                window.geometry = restore;
            }
            window.layout = WindowLayout::Floating;
            window.restore = None;
        }
        true
    }

    /// Current presentation layout, if the window is managed.
    pub fn window_layout(&self, id: u64) -> Option<WindowLayout> {
        self.windows.get(&id).map(|window| window.layout)
    }

    /// Ask a window's client to close (polite `close` request). The
    /// client unmaps itself; `reconcile` drops it on the next tick and
    /// focus falls back. False for unknown ids.
    pub fn close_window(&self, id: u64) -> bool {
        let Some(window) = self.windows.get(&id) else {
            return false;
        };
        match window.surface.underlying_surface() {
            WindowSurface::Wayland(toplevel) => {
                toplevel.send_close();
            }
            #[cfg(feature = "xwayland")]
            WindowSurface::X11(surface) => {
                if surface.close().is_err() {
                    eprintln!("roost-compositor: window {id} X11 close refused");
                    return false;
                }
            }
        }
        eprintln!("roost-compositor: window {id} close requested");
        true
    }

    /// Close the focused window, if any.
    pub fn close_focused(&self) -> bool {
        self.model.focused().is_some_and(|id| self.close_window(id))
    }

    /// Work area a maximized window fills: the output minus the shell
    /// panel strip ([`WORK_AREA_TOP`]).
    fn work_area(state: &State) -> Rectangle<i32, Logical> {
        let primary = state.primary_size();
        let (w, h) = (primary.w.max(0), primary.h.max(0));
        Rectangle {
            loc: (0, WORK_AREA_TOP).into(),
            size: (w, (h - WORK_AREA_TOP).max(0)).into(),
        }
    }

    /// One work-area half for tiled windows.
    fn tile_area(state: &State, side: TileSide) -> Rectangle<i32, Logical> {
        let work = Self::work_area(state);
        let half = work.size.w / 2;
        let (x, w) = match side {
            TileSide::Left => (work.loc.x, half),
            TileSide::Right => (work.loc.x + work.size.w - half, half),
        };
        Rectangle {
            loc: (x, work.loc.y).into(),
            size: (w, work.size.h).into(),
        }
    }

    /// Whole output, including the panel strip, for fullscreen windows.
    fn fullscreen_area(state: &State) -> Rectangle<i32, Logical> {
        let primary = state.primary_size();
        Rectangle {
            loc: (0, 0).into(),
            size: (primary.w.max(0), primary.h.max(0)).into(),
        }
    }

    /// Gap between scroll-mode strip columns (niri default).
    const STRIP_GAP: i32 = 16;
    /// Strip column width presets as work-area fractions, cycled with
    /// Super+R (niri preset-column-widths).
    const STRIP_PRESETS: [f64; 3] = [1.0 / 3.0, 1.0 / 2.0, 2.0 / 3.0];
    /// Default preset slot: 1/2 of the work area (niri
    /// default-column-width).
    const STRIP_DEFAULT_PRESET: usize = 1;

    /// Work-area fraction for `id`'s strip column: its chosen preset
    /// or the default.
    fn strip_proportion(&self, id: u64) -> f64 {
        let slot = self
            .windows
            .get(&id)
            .and_then(|window| window.preset)
            .unwrap_or(Self::STRIP_DEFAULT_PRESET);
        Self::STRIP_PRESETS[slot % Self::STRIP_PRESETS.len()]
    }

    /// One column width for a work area at a proportion (niri
    /// gaps-twice formula), shared by layout and scroll clamping.
    fn strip_width(work_w: i32, proportion: f64) -> i32 {
        (((work_w - Self::STRIP_GAP) as f64 * proportion) as i32 - Self::STRIP_GAP).max(1)
    }

    /// Current session mode (scrollable-tiling spec).
    pub fn session_mode(&self) -> SessionMode {
        self.mode
    }

    /// Same-workspace strip columns bottom-to-top with resolved
    /// widths, for `id`'s workspace (falls back to active).
    fn strip_columns(&self, state: &State, id: u64) -> Vec<(u64, i32)> {
        let work = Self::work_area(state);
        let workspace = self
            .model
            .window(id)
            .map(|entry| entry.workspace)
            .unwrap_or_else(|| self.model.active_workspace());
        self.stacking
            .iter()
            .copied()
            .filter(|other| {
                self.model
                    .window(*other)
                    .is_some_and(|entry| entry.workspace == workspace)
            })
            .map(|other| {
                let width = Self::strip_width(work.size.w, self.strip_proportion(other));
                (other, width)
            })
            .collect()
    }

    /// Total strip extent (columns plus inner gaps) for clamping.
    fn strip_total(total_widths: &[(u64, i32)]) -> i32 {
        if total_widths.is_empty() {
            0
        } else {
            total_widths
                .iter()
                .map(|(_, w)| w + Self::STRIP_GAP)
                .sum::<i32>()
                - Self::STRIP_GAP
        }
    }

    /// One strip column rectangle for `id`: the work area's height less
    /// a gap above and below, preset-proportion width with gaps between
    /// columns and at the strip's ends (niri gaps-twice formula).
    /// Columns past the right edge overflow; the strip never squeezes.
    fn strip_column_area(&self, state: &State, id: u64) -> Rectangle<i32, Logical> {
        let work = Self::work_area(state);
        let columns = self.strip_columns(state, id);
        let mut x = work.loc.x + Self::STRIP_GAP - self.strip_offset as i32;
        let mut width = Self::strip_width(work.size.w, self.strip_proportion(id));
        for (other, w) in &columns {
            if *other == id {
                width = *w;
                break;
            }
            x += w + Self::STRIP_GAP;
        }
        Rectangle {
            loc: (x, work.loc.y + Self::STRIP_GAP).into(),
            size: (width, (work.size.h - 2 * Self::STRIP_GAP).max(1)).into(),
        }
    }

    /// The farthest the view scrolls: the strip with its end gaps,
    /// less the view.
    fn strip_max_offset(&self, state: &State) -> f64 {
        let work = Self::work_area(state);
        let widths: Vec<(u64, i32)> = self
            .strip_order()
            .iter()
            .map(|id| {
                (
                    *id,
                    Self::strip_width(work.size.w, self.strip_proportion(*id)),
                )
            })
            .collect();
        let total = Self::strip_total(&widths) + 2 * Self::STRIP_GAP;
        (total - work.size.w).max(0) as f64
    }

    /// niri's default view motion (`center-focused-column "never"`):
    /// scroll the least that shows the focused column whole, a gap from
    /// the edge it was beyond.
    fn follow_focus(&mut self, state: &mut State) {
        if self.mode != SessionMode::Scroll {
            return;
        }
        let Some(focused) = self.model.focused() else {
            return;
        };
        let work = Self::work_area(state);
        let mut start = 0;
        let mut width = None;
        for id in self.strip_order() {
            let w = Self::strip_width(work.size.w, self.strip_proportion(id));
            if id == focused {
                width = Some(w);
                break;
            }
            start += w + Self::STRIP_GAP;
        }
        let Some(width) = width else { return };
        // Column `start` sits at gap + start - offset in the view.
        let left = f64::from(start);
        let right = f64::from(start + width + 2 * Self::STRIP_GAP - work.size.w);
        let mut offset = self.strip_offset;
        if offset > left {
            offset = left;
        } else if offset < right {
            offset = right;
        }
        self.strip_offset = offset.clamp(0.0, self.strip_max_offset(state));
    }

    /// Current strip view offset target (scroll mode): where the view
    /// is going, which layout and tests use.
    pub fn strip_offset(&self) -> f64 {
        self.strip_offset
    }

    /// Where the strip view is drawn this frame; equals
    /// [`strip_offset`](Self::strip_offset) once the spring settles.
    pub fn strip_view(&self) -> f64 {
        self.strip_shown
    }

    /// Whether the strip view is still moving toward its target.
    pub fn strip_animating(&self) -> bool {
        self.mode == SessionMode::Scroll && self.strip_shown != self.strip_offset
    }

    /// Jump the drawn view onto the target (no animation).
    fn snap_strip_view(&mut self) {
        self.strip_shown = self.strip_offset;
        self.strip_anim = None;
    }

    /// Apply the live animation preference and finish motion already in flight.
    pub fn set_animations_enabled(&mut self, enabled: bool) {
        self.animations_enabled = enabled;
        if !enabled {
            self.snap_strip_view();
            for animation in self.column_animations.values_mut() {
                animation.step(0.0, false);
            }
        }
    }

    /// Advance the drawn strip view `dt` seconds toward the target on
    /// niri's default view-movement spring (critically damped,
    /// stiffness 800, epsilon 0.0001). A target that moved mid-flight
    /// restarts the spring from the drawn position with its current
    /// velocity, as niri does. Called once per frame by the runtime;
    /// returns whether the view is still moving (frames keep coming
    /// only while it does).
    pub fn step_strip_view(&mut self, dt: f64) -> bool {
        use crate::spring::Spring;
        let columns: Vec<_> = self
            .visible_entries()
            .into_iter()
            .filter(|(id, _, _)| self.window_layout(*id) == Some(WindowLayout::Strip))
            .map(|(id, _, mut rect)| {
                rect.loc.x += self.strip_offset.round() as i32;
                (id, rect)
            })
            .collect();
        self.column_animations
            .retain(|id, _| columns.iter().any(|(other, _)| id == other));
        let mut columns_moving = false;
        for (id, rect) in columns {
            let animation = self
                .column_animations
                .entry(id)
                .or_insert_with(|| crate::animation::ColumnSpring::new(rect, rect));
            animation.retarget(rect);
            columns_moving |= animation.step(dt, self.animations_enabled);
        }
        if !self.animations_enabled {
            self.snap_strip_view();
            return false;
        }
        if self.mode != SessionMode::Scroll {
            self.snap_strip_view();
            return false;
        }
        let target = self.strip_offset;
        let (spring, t) = match self.strip_anim {
            Some((spring, t)) if spring.to == target => (spring, t + dt.max(0.0)),
            Some((spring, t)) => (
                Spring::view_movement(spring.value_at(t), target, spring.velocity_at(t)),
                0.0,
            ),
            None if self.strip_shown != target => {
                (Spring::view_movement(self.strip_shown, target, 0.0), 0.0)
            }
            None => return columns_moving,
        };
        if spring.done_at(t) {
            self.snap_strip_view();
            return columns_moving;
        }
        self.strip_shown = spring.value_at(t);
        self.strip_anim = Some((spring, t));
        true
    }

    /// Scroll the strip by axis amounts (positive moves the view
    /// right, toward later columns). Only acts in scroll mode; the
    /// offset clamps to the strip overflow and focus stays on the
    /// same window. False when not scrolling.
    fn scroll_strip(&mut self, state: &mut State, horizontal: f64, vertical: f64) -> bool {
        if self.mode != SessionMode::Scroll {
            return false;
        }
        let work = Self::work_area(state);
        let widths: Vec<(u64, i32)> = self
            .strip_order()
            .iter()
            .map(|id| {
                (
                    *id,
                    Self::strip_width(work.size.w, self.strip_proportion(*id)),
                )
            })
            .collect();
        let _ = (&widths, work);
        let max = self.strip_max_offset(state);
        let next = (self.strip_offset + horizontal + vertical).clamp(0.0, max);
        if next == self.strip_offset {
            return true;
        }
        self.strip_offset = next;
        // Wheel and touchpad scrolling is direct manipulation: the view
        // follows the fingers without a spring (niri's gesture too).
        self.snap_strip_view();
        self.relayout_strip(state);
        true
    }

    /// Step the focused column through the width presets (positive
    /// cycles forward, negative backward, wrapping). Only strip
    /// columns participate; other layouts and gnome mode are
    /// untouched. False with no focused strip column.
    fn cycle_preset(&mut self, state: &mut State, direction: i32) -> bool {
        let Some(focused) = self.model.focused() else {
            return false;
        };
        if self.mode != SessionMode::Scroll
            || self.window_layout(focused) != Some(WindowLayout::Strip)
        {
            return false;
        }
        let count = Self::STRIP_PRESETS.len() as i32;
        let current = self
            .windows
            .get(&focused)
            .and_then(|window| window.preset)
            .unwrap_or(Self::STRIP_DEFAULT_PRESET) as i32;
        let next = (current + direction).rem_euclid(count) as usize;
        if let Some(window) = self.windows.get_mut(&focused) {
            window.preset = Some(next);
        }
        self.follow_focus(state);
        self.relayout_strip(state);
        true
    }

    /// Active-workspace windows bottom-to-top: the strip order focus
    /// motion and slot moves operate on.
    fn strip_order(&self) -> Vec<u64> {
        let active = self.model.active_workspace();
        self.stacking
            .iter()
            .copied()
            .filter(|id| {
                self.model
                    .window(*id)
                    .is_some_and(|entry| entry.workspace == active)
            })
            .collect()
    }

    /// Move strip focus one column left/right (negative/positive
    /// delta), clamped at the ends. Focus never reorders the strip:
    /// column order is independent of focus. False with no windows.
    fn focus_neighbor_in_strip(&mut self, state: &mut State, delta: i32) -> bool {
        let order = self.strip_order();
        if order.is_empty() {
            return false;
        }
        let pos = self
            .model
            .focused()
            .and_then(|focused| order.iter().position(|id| *id == focused))
            .unwrap_or(0) as i32;
        let next = pos.saturating_add(delta).clamp(0, order.len() as i32 - 1) as usize;
        self.apply_focus(state, Some(order[next]));
        true
    }

    /// Move the focused window one strip slot left/right, following it
    /// with focus. Only the two swapped columns change geometry; the
    /// rest of the strip is untouched. False with no movable window.
    fn move_focused_in_strip(&mut self, state: &mut State, delta: i32) -> bool {
        let Some(focused) = self.model.focused() else {
            return false;
        };
        let active = self.model.active_workspace();
        let off_workspace = self
            .model
            .window(focused)
            .map(|entry| entry.workspace != active)
            .unwrap_or(true);
        if off_workspace {
            return false;
        }
        let positions: Vec<usize> = self
            .stacking
            .iter()
            .enumerate()
            .filter(|(_, id)| {
                self.model
                    .window(**id)
                    .is_some_and(|entry| entry.workspace == active)
            })
            .map(|(index, _)| index)
            .collect();
        let Some(slot) = positions
            .iter()
            .position(|index| self.stacking[*index] == focused)
        else {
            return false;
        };
        let other = slot
            .saturating_add_signed(delta as isize)
            .clamp(0, positions.len() - 1);
        if other == slot {
            return true;
        }
        self.stacking.swap(positions[slot], positions[other]);
        self.follow_focus(state);
        self.relayout_strip(state);
        true
    }

    /// Re-resolve every strip column (after slot moves, preset
    /// steps, unmaps). Windows in other managed layouts keep theirs.
    /// The view offset clamps to the new overflow first so a shrink
    /// never strands blank space at the strip end.
    fn relayout_strip(&mut self, state: &mut State) {
        let work = Self::work_area(state);
        let widths: Vec<(u64, i32)> = self
            .strip_order()
            .iter()
            .map(|id| {
                (
                    *id,
                    Self::strip_width(work.size.w, self.strip_proportion(*id)),
                )
            })
            .collect();
        let _ = (&widths, work);
        self.strip_offset = self.strip_offset.clamp(0.0, self.strip_max_offset(state));
        let ids: Vec<u64> = self.windows.keys().copied().collect();
        for id in ids {
            if self.window_layout(id) == Some(WindowLayout::Strip) {
                self.apply_layout(state, id, WindowLayout::Strip);
            }
        }
    }

    /// Flip the whole session between floating and strip modes,
    /// re-laying out every window in place. Entering the strip stashes
    /// floating geometry through the existing stash-once path;
    /// leaving restores every strip column. Windows in other managed
    /// layouts keep their layout (forced membership is later work).
    pub fn set_scroll(&mut self, state: &mut State, scroll: bool) -> bool {
        self.mode = if scroll {
            SessionMode::Scroll
        } else {
            SessionMode::Gnome
        };
        if scroll {
            self.strip_offset = 0.0;
            self.follow_focus(state);
            // Entering the strip lays it out in place: nothing to
            // animate from.
            self.snap_strip_view();
            let ids: Vec<u64> = self.windows.keys().copied().collect();
            for id in ids {
                self.apply_layout(state, id, WindowLayout::Strip);
            }
        } else {
            let ids: Vec<u64> = self.windows.keys().copied().collect();
            for id in ids {
                if self.window_layout(id) == Some(WindowLayout::Strip) {
                    self.restore_window(id);
                }
            }
        }
        // Greppable end-to-end signal for the CI proof: the mode
        // flipped (strip and floating can look alike with one narrow
        // window, so pixels alone cannot prove it).
        eprintln!(
            "roost-compositor: session mode now {}",
            if scroll { "scroll" } else { "gnome" }
        );
        true
    }

    /// Maximize a window into the work area, stashing its floating
    /// geometry for restore. Idempotent; false for unknown ids.
    pub fn set_maximized(&mut self, state: &mut State, id: u64, maximized: bool) -> bool {
        // Forced strip membership: in scroll mode both directions
        // resolve to a column (un-maximizing mid-scroll stays a
        // column); the stashed floating geometry survives underneath
        // for the toggle back.
        if self.mode == SessionMode::Scroll && self.windows.contains_key(&id) {
            return self.apply_layout(state, id, WindowLayout::Strip);
        }
        if maximized {
            self.apply_layout(state, id, WindowLayout::Maximized)
        } else {
            self.restore_window(id)
        }
    }

    /// Cover the output with a window, stashing its floating geometry.
    /// Idempotent; false for unknown ids.
    pub fn set_fullscreen(&mut self, state: &mut State, id: u64, fullscreen: bool) -> bool {
        // Forced strip membership, like maximize: fullscreen changes
        // mid-scroll stay columns instead of covering the screen.
        if self.mode == SessionMode::Scroll && self.windows.contains_key(&id) {
            return self.apply_layout(state, id, WindowLayout::Strip);
        }
        if fullscreen {
            self.apply_layout(state, id, WindowLayout::Fullscreen)
        } else {
            self.restore_window(id)
        }
    }

    /// Tile a window into one work-area half, stashing its floating
    /// geometry. Idempotent; false for unknown ids.
    pub fn set_tiled(&mut self, state: &mut State, id: u64, side: TileSide) -> bool {
        // Forced strip membership: tiling mid-scroll is already a
        // column, so re-resolve instead of splitting the work area.
        if self.mode == SessionMode::Scroll && self.windows.contains_key(&id) {
            return self.apply_layout(state, id, WindowLayout::Strip);
        }
        self.apply_layout(state, id, WindowLayout::Tiled(side))
    }

    /// Return a window to floating and re-advertise its geometry.
    /// False for unknown ids.
    pub fn restore_window(&mut self, id: u64) -> bool {
        let changed = self
            .window_layout(id)
            .is_some_and(|layout| layout != WindowLayout::Floating);
        if !self.restore_layout(id) {
            return false;
        }
        self.configure(id, self.model.focused() == Some(id));
        if changed {
            eprintln!("roost-compositor: window {id} Floating");
        }
        true
    }

    /// Apply one managed layout: stash the floating geometry once
    /// (moving between managed layouts keeps the original restore),
    /// set the computed geometry, and advertise the new state.
    fn apply_layout(&mut self, state: &mut State, id: u64, layout: WindowLayout) -> bool {
        self.settle(id);
        let area = match layout {
            WindowLayout::Floating => return self.restore_window(id),
            WindowLayout::Maximized => Self::work_area(state),
            WindowLayout::Tiled(side) => Self::tile_area(state, side),
            WindowLayout::Fullscreen => Self::fullscreen_area(state),
            WindowLayout::Strip => self.strip_column_area(state, id),
        };
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        if window.layout == WindowLayout::Floating {
            window.restore = Some(window.geometry);
        }
        window.layout = layout;
        window.geometry = area;
        self.configure(id, self.model.focused() == Some(id));
        eprintln!("roost-compositor: window {id} {layout:?}");
        true
    }

    /// Shift `geo` from the removed output's slice into the
    /// survivor's, preserving size and clamping inside the survivor
    /// bounds (an oversized window pins at the survivor origin).
    fn shift_into_survivor(
        geo: Rectangle<i32, Logical>,
        removed_loc: (i32, i32),
        survivor_loc: (i32, i32),
        survivor_size: Size<i32, Logical>,
    ) -> Rectangle<i32, Logical> {
        let x = (survivor_loc.0 + geo.loc.x - removed_loc.0).clamp(
            survivor_loc.0,
            (survivor_loc.0 + survivor_size.w - geo.size.w).max(survivor_loc.0),
        );
        let y = (survivor_loc.1 + geo.loc.y - removed_loc.1).clamp(
            survivor_loc.1,
            (survivor_loc.1 + survivor_size.h - geo.size.h).max(survivor_loc.1),
        );
        Rectangle {
            loc: (x, y).into(),
            size: geo.size,
        }
    }

    /// Whether `geo`'s center sits inside the `(loc, size)` slice:
    /// the test for "this window lives on that output".
    fn center_in_slice(
        geo: Rectangle<i32, Logical>,
        loc: (i32, i32),
        size: Size<i32, Logical>,
    ) -> bool {
        let (cx, cy) = (geo.loc.x + geo.size.w / 2, geo.loc.y + geo.size.h / 2);
        cx >= loc.0 && cx < loc.0 + size.w && cy >= loc.1 && cy < loc.1 + size.h
    }

    /// Migrate windows off a removed output onto a live one, before
    /// its inventory entry drops: call this, then
    /// [`State::remove_output`](crate::State::remove_output), then
    /// [`reapply_derived_layouts`](Self::reapply_derived_layouts) when
    /// the primary changed.
    ///
    /// Windows carry no output affinity (they live in workspaces),
    /// so migration is geometric: floating windows centered on the
    /// removed slice shift into the survivor slice (preferring the
    /// primary) with size preserved. Derived layouts (maximized,
    /// tiled, fullscreen, strip) are positional only through the
    /// primary areas, so they move in the re-apply step after
    /// failover — re-applying here would size them for the dying
    /// primary. Stashed restore rects shift with their windows.
    /// Focus and workspace membership never change, so no window is
    /// lost — only positions move. Returns how many windows moved.
    /// Unknown names and a missing survivor (removing the last
    /// output) migrate nothing.
    pub fn migrate_output_windows(&mut self, state: &mut State, removed: &str) -> usize {
        let Some(gone) = state
            .outputs
            .iter()
            .find(|entry| entry.name == removed)
            .cloned()
        else {
            return 0;
        };
        let Some(live) = state
            .outputs
            .iter()
            .find(|entry| entry.primary && entry.name != removed)
            .or_else(|| state.outputs.iter().find(|entry| entry.name != removed))
            .cloned()
        else {
            return 0;
        };
        let mut moved = 0;
        let mut ids: Vec<u64> = self.windows.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(layout) = self.windows.get(&id).map(|window| window.layout) else {
                continue;
            };
            if layout != WindowLayout::Floating {
                continue;
            }
            let mut shifted = false;
            if let Some(window) = self.windows.get_mut(&id) {
                if Self::center_in_slice(window.geometry, gone.loc, gone.size) {
                    window.geometry =
                        Self::shift_into_survivor(window.geometry, gone.loc, live.loc, live.size);
                    shifted = true;
                }
                if let Some(restore) = window.restore {
                    if Self::center_in_slice(restore, gone.loc, gone.size) {
                        window.restore = Some(Self::shift_into_survivor(
                            restore, gone.loc, live.loc, live.size,
                        ));
                        shifted = true;
                    }
                }
            }
            moved += usize::from(shifted);
        }
        moved
    }

    /// Re-resolve every derived-layout window (maximized, tiled,
    /// fullscreen, strip) against the current primary areas: call
    /// after [`State::remove_output`](crate::State::remove_output)
    /// when the primary changed, so no window keeps the dead
    /// primary's geometry. Floating windows are untouched (migration
    /// already placed them). Returns how many windows re-applied.
    pub fn reapply_derived_layouts(&mut self, state: &mut State) -> usize {
        let mut ids: Vec<u64> = self.windows.keys().copied().collect();
        ids.sort_unstable();
        let mut applied = 0;
        for id in ids {
            let Some(layout) = self.windows.get(&id).map(|window| window.layout) else {
                continue;
            };
            if layout != WindowLayout::Floating && self.apply_layout(state, id, layout) {
                applied += 1;
            }
        }
        applied
    }

    /// Move a window to another workspace (registered if new). Focus
    /// follows the window when it leaves the active workspace.
    pub fn move_to_workspace(&mut self, state: &mut State, id: u64, workspace: u32) -> bool {
        if !self.windows.contains_key(&id) {
            return false;
        }
        if !self.model.update(
            id,
            WindowUpdate {
                workspace: Some(workspace),
                ..Default::default()
            },
        ) {
            return false;
        }
        if Some(id) == self.model.focused()
            && self
                .model
                .window(id)
                .is_some_and(|entry| entry.workspace != self.model.active_workspace())
        {
            self.focus_topmost(state, self.model.active_workspace());
        }
        true
    }

    /// Create the workspace represented by an overview drop placeholder.
    pub fn insert_workspace_and_move(&mut self, state: &mut State, id: u64, at: u32) -> bool {
        if !self.windows.contains_key(&id) || !self.model.insert_workspace_and_move(id, at) {
            return false;
        }
        self.focus_topmost(state, self.model.active_workspace());
        true
    }

    /// Switch the active workspace, focusing its topmost window (or
    /// nothing when empty). Returns false for unknown ids.
    pub fn switch_workspace(&mut self, state: &mut State, workspace: u32) -> bool {
        if !self.model.workspaces().contains(&workspace) {
            return false;
        }
        if !self.model.set_active_workspace(workspace) {
            return false;
        }
        self.focus_topmost(state, workspace);
        true
    }

    /// Switch relative to the active workspace (-1 previous, +1 next)
    /// over the sorted known ids. Unknown target ids are created,
    /// matching dynamic-workspace policy.
    pub fn switch_relative(&mut self, state: &mut State, delta: i32) -> bool {
        let known = self.model.workspaces();
        let active = self.model.active_workspace();
        let pos = known.iter().position(|id| *id == active);
        let target = match (pos, delta) {
            (Some(i), _) => {
                let next = i as i32 + delta;
                if next < 0 {
                    return false;
                }
                known.get(next as usize).copied().or_else(|| {
                    // Past the last workspace: create the next id.
                    (delta > 0).then(|| known.iter().max().copied().unwrap_or(0) + 1)
                })
            }
            // Active unknown cannot happen (always registered), but a
            // first workspace still needs creation on +1.
            (None, _) if delta > 0 => Some(active + 1),
            (None, _) => None,
        };
        let Some(target) = target else {
            return false;
        };
        if !self.model.set_active_workspace(target) {
            return false;
        }
        self.focus_topmost(state, target);
        true
    }

    /// Move the focused window one workspace over and follow it.
    /// Returns false with no focused window.
    pub fn move_focused_relative(&mut self, state: &mut State, delta: i32) -> bool {
        let Some(id) = self.model.focused() else {
            return false;
        };
        let Some(entry) = self.model.window(id) else {
            return false;
        };
        let current = entry.workspace;
        let target = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta.unsigned_abs())
        };
        if !self.move_to_workspace(state, id, target) {
            return false;
        }
        if !self.model.set_active_workspace(target) {
            return false;
        }
        self.apply_focus(state, Some(id));
        true
    }

    /// Keep physical modifier holds current even when a lock, blanking shield
    /// or recovery overlay consumes the event. This never delivers client input.
    pub fn note_modifiers(&mut self, input: &ManagerInput) {
        if let ManagerInput::Key {
            keycode, pressed, ..
        } = input
        {
            self.track_workspace_modifiers(*keycode, *pressed);
            self.track_switcher_modifiers(*keycode, *pressed);
        }
    }

    /// Account for a consumed key without sending it to any client.
    pub fn discard_key_input(&mut self, state: &mut State, input: &ManagerInput) {
        self.note_modifiers(input);
        if let (
            Some(keyboard),
            ManagerInput::Key {
                keycode, pressed, ..
            },
        ) = (self.keyboard.clone(), input)
        {
            if !*pressed {
                self.accel_held.retain(|held| held != keycode);
            }
            keyboard.input_discard(
                state,
                keycode.saturating_add(XKB_X11_OFFSET).into(),
                if *pressed {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                },
            );
        }
    }

    /// Track Super/Shift hold state for workspace keybindings. The
    /// modifier events themselves still forward to clients.
    fn track_workspace_modifiers(&mut self, keycode: u32, pressed: bool) {
        if keycode == SUPER_LEFT_KEYCODE || keycode == SUPER_RIGHT_KEYCODE {
            self.super_held = pressed;
        } else if keycode == SHIFT_LEFT_KEYCODE || keycode == SHIFT_RIGHT_KEYCODE {
            self.shift_held = pressed;
        }
    }

    /// Focus the topmost window on `workspace`, or unfocus when empty.
    fn focus_topmost(&mut self, state: &mut State, workspace: u32) {
        let topmost = self.stacking.iter().rev().copied().find(|id| {
            (self
                .model
                .window(*id)
                .is_some_and(|entry| entry.workspace == workspace)
                || self.windows.get(id).is_some_and(|w| w.sticky))
                && self.windows.get(id).is_some_and(|w| !w.minimized)
        });
        // In the overview a workspace switch moves the activated look
        // and the window restored on close; the keys stay with the
        // overview.
        if self.overview_held.is_some() {
            self.focus_parked(topmost);
            return;
        }
        self.apply_focus(state, topmost);
    }

    /// Geometry of one window, for frame production and tests.
    pub fn geometry(&self, id: u64) -> Option<Rectangle<i32, Logical>> {
        self.windows.get(&id).map(|w| w.geometry)
    }
}

/// Read the client's current title from the toplevel role data.
fn read_title(surface: &ToplevelSurface) -> Option<String> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().unwrap().title.clone())
    })
}

/// Read the client's current app id from the toplevel role data.
fn read_app_id(surface: &ToplevelSurface) -> Option<String> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().unwrap().app_id.clone())
    })
}

/// Whether the client marked this toplevel a modal dialog (xdg-dialog).
fn read_modal(surface: &ToplevelSurface) -> bool {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .is_some_and(|data| data.lock().unwrap().modal)
    })
}

/// Read the client's transient parent from the toplevel role data.
fn read_parent(surface: &ToplevelSurface) -> Option<WlSurface> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().unwrap().parent.clone())
    })
}

fn contains(geometry: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> bool {
    let x = geometry.loc.x as f64;
    let y = geometry.loc.y as f64;
    pos.x >= x
        && pos.y >= y
        && pos.x < x + geometry.size.w as f64
        && pos.y < y + geometry.size.h as f64
}

/// Backend input event kinds this manager consumes from the runtime.
#[derive(Debug, Clone, Copy)]
pub enum ManagerInput {
    Key {
        keycode: u32,
        pressed: bool,
        time: u32,
    },
    Motion {
        pos: Point<f64, Logical>,
        time: u32,
    },
    Button {
        button: u32,
        pressed: bool,
        time: u32,
    },
    /// A native relative device crossed the pressure threshold. Never
    /// constructed from absolute motion or accepted from remote clients.
    CornerPressure {
        pos: Point<f64, Logical>,
        time: u32,
    },
    /// Pointer axis (wheel / scroll) motion, in backend units (v120
    /// for wheels, pixels for continuous devices). Only scroll mode
    /// consumes it; everywhere else it is dropped as before.
    Axis {
        horizontal: f64,
        vertical: f64,
        time: u32,
    },
    /// Raw pointer motion for relative-pointer clients (games, remote
    /// desktop); comes alongside `Motion` from devices that have it.
    RelativeMotion {
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        /// Microseconds, the protocol's precision.
        utime: u64,
    },
    /// Touchpad swipe gesture phases (#60). Three-finger swipes are
    /// the shell's (overview, workspaces); others reach the client.
    SwipeBegin {
        fingers: u32,
        time: u32,
    },
    SwipeUpdate {
        delta: Point<f64, Logical>,
        time: u32,
    },
    SwipeEnd {
        cancelled: bool,
        time: u32,
    },
}

/// What a finished three-finger touchpad swipe does (GNOME 51).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeAction {
    /// Swipe up: open the overview.
    OpenOverview,
    /// Swipe down: close it.
    CloseOverview,
    /// Fingers move left: the next workspace slides in (content follows
    /// the fingers, as on GNOME).
    NextWorkspace,
    /// Fingers move right: the previous workspace.
    PreviousWorkspace,
}

/// Fingers GNOME reserves for its own swipes.
pub const SHELL_SWIPE_FINGERS: u32 = 3;
/// Distance a swipe must travel to act (logical pixels).
pub const SWIPE_THRESHOLD: f64 = 100.0;

/// Classify a finished swipe by its dominant axis; short swipes do
/// nothing.
pub fn swipe_action(dx: f64, dy: f64) -> Option<SwipeAction> {
    if dx.abs().max(dy.abs()) < SWIPE_THRESHOLD {
        return None;
    }
    Some(if dy.abs() >= dx.abs() {
        if dy < 0.0 {
            SwipeAction::OpenOverview
        } else {
            SwipeAction::CloseOverview
        }
    } else if dx < 0.0 {
        SwipeAction::NextWorkspace
    } else {
        SwipeAction::PreviousWorkspace
    })
}

/// X11 keycodes are kernel evdev numbers plus 8; the winit backend
/// normalizes back to evdev in [`translate_input`] so every table in
/// this file stays in evdev keycodes on all backends.
pub const XKB_X11_OFFSET: u32 = 8;

/// Overview trigger keycodes (evdev, 002 R1).
pub const SUPER_LEFT_KEYCODE: u32 = 125;
/// Space (evdev): Super+Space switches input source.
pub const SPACE_KEYCODE: u32 = 57;
pub const SUPER_RIGHT_KEYCODE: u32 = 126;
pub const ESCAPE_KEYCODE: u32 = 1;
/// Workspace keybindings (evdev): Super+PageUp/PageDown switches the
/// active workspace, holding Shift as well moves the focused window
/// and follows it.
pub const PAGE_UP_KEYCODE: u32 = 104;
pub const PAGE_DOWN_KEYCODE: u32 = 109;
pub const SHIFT_LEFT_KEYCODE: u32 = 42;
pub const SHIFT_RIGHT_KEYCODE: u32 = 54;
/// Alt-Tab switcher keybindings (evdev): Tab while Alt is held steps
/// the shell switcher (Shift+Tab steps back), Alt release commits,
/// Escape cancels. The Tab press is consumed; Alt press/release still
/// reach clients so app modifiers never stick.
pub const TAB_KEYCODE: u32 = 15;
pub const ALT_LEFT_KEYCODE: u32 = 56;
pub const ALT_RIGHT_KEYCODE: u32 = 100;
/// Window-action keybindings (evdev, GNOME shape): Super+Up maximizes,
/// Super+Down restores, Super+Left/Right tiles that half (repeat
/// toggles back to floating). Presses are consumed.
pub const ARROW_UP_KEYCODE: u32 = 103;
pub const ARROW_DOWN_KEYCODE: u32 = 108;
pub const ARROW_LEFT_KEYCODE: u32 = 105;
pub const ARROW_RIGHT_KEYCODE: u32 = 106;
/// Close-window keybinding (evdev, GNOME shape): Alt+F4 asks the
/// focused client to close (polite `close` request; the client unmaps
/// itself). The press is consumed; releases still reach clients.
pub const F4_KEYCODE: u32 = 62;
/// Session-mode toggle key (evdev): Super+Shift+T flips the whole
/// session between floating and strip modes. The press is consumed;
/// the release still reaches clients.
pub const T_KEYCODE: u32 = 20;
/// GNOME's `switch-group` key (Above_Tab: the key above Tab, evdev
/// KEY_GRAVE) and the keys the open switcher acts on.
pub const GRAVE_KEYCODE: u32 = 41;
pub const Q_KEYCODE: u32 = 16;
pub const W_KEYCODE: u32 = 17;
pub const CTRL_LEFT_KEYCODE: u32 = 29;
pub const CTRL_RIGHT_KEYCODE: u32 = 97;

/// The keysym the open switcher hears for an evdev key it acts on
/// (GNOME's AppSwitcherPopup: arrows, Q, W, F4).
fn switcher_keysym(keycode: u32) -> Option<u32> {
    Some(match keycode {
        ARROW_LEFT_KEYCODE => switcher_keys::LEFT,
        ARROW_UP_KEYCODE => switcher_keys::UP,
        ARROW_RIGHT_KEYCODE => switcher_keys::RIGHT,
        ARROW_DOWN_KEYCODE => switcher_keys::DOWN,
        F4_KEYCODE => switcher_keys::F4,
        Q_KEYCODE => switcher_keys::Q,
        W_KEYCODE => switcher_keys::W,
        _ => return None,
    })
}
/// Strip preset-cycle key (evdev): Super+R steps the focused column
/// forward through the width presets, Super+Shift+R backward. Only
/// consumed in scroll mode; in gnome mode the press reaches clients
/// exactly as before (no R binding exists today).
pub const R_KEYCODE: u32 = 19;
/// evdev KEY_H: Super+H hides the focused window (GNOME's minimize).
pub const H_KEYCODE: u32 = 35;
/// Top inset of the maximized/tiled work area: the shell panel strip.
/// Derived from [`PANEL_HEIGHT`] (the compositor↔shell contract).
pub const WORK_AREA_TOP: i32 = PANEL_HEIGHT as i32;
/// Hot-corner trigger region in logical pixels from the top-left.
pub const HOT_CORNER_PX: f64 = 8.0;
/// Activities-strip trigger height: the top strip the panel owns.
/// Derived from [`PANEL_HEIGHT`] (the compositor↔shell contract).
pub const ACTIVITIES_STRIP_PX: f64 = PANEL_HEIGHT as f64;
/// Activities-strip trigger width: only presses on the Activities
/// control at the strip's left end toggle the overview, as in GNOME.
/// Clicks on the clock, indicators, or tray belong to the shell.
pub const ACTIVITIES_WIDTH_PX: f64 = 96.0;

/// What an input event means for the overview (002 R1), decided purely
/// from the event plus the current intent and pointer height. The
/// runtime applies the action to the hub and still forwards the event,
/// except Escape-closes which it consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerAction {
    /// No overview meaning; normal routing.
    None,
    /// Flip the overview intent.
    Toggle,
    /// Open the overview.
    Open,
}

/// Tap-detecting overview trigger state: a lone Super press+release
/// toggles, while any other input in between cancels the tap so client
/// Super-combos keep working. Hot corner and Activities strip open.
#[derive(Debug, Default)]
pub struct TriggerState {
    super_armed: bool,
    /// GNOME's `enable-hot-corners` off (default on, as in GNOME).
    hot_corner_off: bool,
    right_to_left: bool,
    pressure_only: bool,
}

impl TriggerState {
    /// Drop a pending Super tap when a modal owner or inhibitor takes input.
    pub fn cancel(&mut self) {
        self.super_armed = false;
    }

    /// Native seats use physical relative barrier pressure, not hover.
    pub fn set_pressure_only(&mut self, enabled: bool) {
        self.pressure_only = enabled;
    }

    /// Follow GNOME's `enable-hot-corners`.
    pub fn set_hot_corner(&mut self, enabled: bool) {
        self.hot_corner_off = !enabled;
    }

    /// Follow the shell's default text direction, including live updates.
    pub fn set_right_to_left(&mut self, right_to_left: bool) {
        self.right_to_left = right_to_left;
    }

    /// Decide the overview action for one input event. `overview_open`
    /// is the hub intent; `pointer` is the last known pointer position
    /// for strip clicks (buttons carry no position).
    pub fn feed(
        &mut self,
        input: &ManagerInput,
        overview_open: bool,
        pointer: Point<f64, Logical>,
    ) -> TriggerAction {
        let corner = matches!(input, ManagerInput::Motion { pos, .. }
            if (0.0..HOT_CORNER_PX).contains(&pos.x)
                && (0.0..HOT_CORNER_PX).contains(&pos.y));
        let strip = (0.0..ACTIVITIES_STRIP_PX).contains(&pointer.y)
            && (0.0..ACTIVITIES_WIDTH_PX).contains(&pointer.x);
        self.feed_regions(input, overview_open, corner, strip)
    }

    /// Decide against the current logical output layout. GNOME 51 always
    /// offers the primary corner; a secondary corner is eligible only when
    /// no other output contains GNOME's direction-dependent approach probes.
    /// The Activities strip belongs to the primary panel. Geometry is borrowed, never cached.
    pub fn feed_on_outputs(
        &mut self,
        input: &ManagerInput,
        overview_open: bool,
        pointer: Point<f64, Logical>,
        outputs: impl Iterator<Item = (Rectangle<i32, Logical>, bool)> + Clone,
    ) -> TriggerAction {
        let outputs = outputs
            .filter(|(rect, _)| rect.size.w > 0 && rect.size.h > 0)
            .enumerate();
        let in_region =
            |pos: Point<f64, Logical>, rect: Rectangle<i32, Logical>, w: f64, h: f64| {
                let x = if self.right_to_left {
                    f64::from(rect.loc.x) + f64::from(rect.size.w) - pos.x
                } else {
                    pos.x - f64::from(rect.loc.x)
                };
                let y = pos.y - f64::from(rect.loc.y);
                let within_x = if self.right_to_left {
                    x > 0.0 && x <= w.min(f64::from(rect.size.w))
                } else {
                    (0.0..w.min(f64::from(rect.size.w))).contains(&x)
                };
                within_x && (0.0..h.min(f64::from(rect.size.h))).contains(&y)
            };
        let corner = if let ManagerInput::Motion { pos, .. } = input {
            outputs.clone().any(|(index, (rect, primary))| {
                if !in_region(*pos, rect, HOT_CORNER_PX, HOT_CORNER_PX) {
                    return false;
                }
                // GNOME Shell 51 layout.js's exact secondary approach probes.
                let corner_x = f64::from(rect.loc.x)
                    + if self.right_to_left {
                        f64::from(rect.size.w)
                    } else {
                        0.0
                    };
                let beside_x = f64::from(rect.loc.x) + if self.right_to_left { 1.0 } else { -1.0 };
                let beside: Point<f64, Logical> = (beside_x, f64::from(rect.loc.y)).into();
                let above: Point<f64, Logical> = (corner_x, f64::from(rect.loc.y) - 1.0).into();
                primary
                    || !outputs.clone().any(|(other_index, (other, _))| {
                        other_index != index
                            && (other.to_f64().contains(beside) || other.to_f64().contains(above))
                    })
            })
        } else {
            false
        };
        let strip = outputs.clone().any(|(_, (rect, primary))| {
            primary && in_region(pointer, rect, ACTIVITIES_WIDTH_PX, ACTIVITIES_STRIP_PX)
        });
        self.feed_regions(input, overview_open, corner, strip)
    }

    fn feed_regions(
        &mut self,
        input: &ManagerInput,
        overview_open: bool,
        corner: bool,
        strip: bool,
    ) -> TriggerAction {
        match *input {
            ManagerInput::Key {
                keycode, pressed, ..
            } if keycode == SUPER_LEFT_KEYCODE || keycode == SUPER_RIGHT_KEYCODE => {
                if pressed {
                    self.super_armed = true;
                    TriggerAction::None
                } else if self.super_armed {
                    self.super_armed = false;
                    TriggerAction::Toggle
                } else {
                    TriggerAction::None
                }
            }
            ManagerInput::Motion { .. } => {
                self.super_armed = false;
                if !self.pressure_only && !self.hot_corner_off && !overview_open && corner {
                    TriggerAction::Open
                } else {
                    TriggerAction::None
                }
            }
            ManagerInput::Button { pressed, .. } => {
                self.super_armed = false;
                if pressed && strip {
                    TriggerAction::Toggle
                } else {
                    TriggerAction::None
                }
            }
            ManagerInput::Key { .. } => {
                self.super_armed = false;
                TriggerAction::None
            }
            ManagerInput::Axis { .. } => {
                self.super_armed = false;
                TriggerAction::None
            }
            ManagerInput::CornerPressure { .. } => {
                self.super_armed = false;
                if !self.hot_corner_off {
                    TriggerAction::Toggle
                } else {
                    TriggerAction::None
                }
            }
            ManagerInput::RelativeMotion { .. }
            | ManagerInput::SwipeBegin { .. }
            | ManagerInput::SwipeUpdate { .. }
            | ManagerInput::SwipeEnd { .. } => TriggerAction::None,
        }
    }
}

impl WindowManager {
    /// GNOME's keyboard settings on the seat (#60): the input sources as
    /// one xkb keymap (layouts in order, cycled with Super+Space) and the
    /// repeat delay and rate. A keymap xkb cannot compile is refused and
    /// the current one stays.
    pub fn apply_keyboard_settings(
        &mut self,
        state: &mut State,
        settings: &roost_shell_control::InputSettings,
    ) {
        let Some(keyboard) = self.keyboard.clone() else {
            return;
        };
        let config = XkbConfig {
            layout: &settings.xkb_layout,
            variant: &settings.xkb_variant,
            options: (!settings.xkb_options.is_empty()).then(|| settings.xkb_options.clone()),
            ..XkbConfig::default()
        };
        if let Err(e) = keyboard.set_xkb_config(state, config) {
            eprintln!(
                "roost-compositor: keymap {}({}) refused: {e:?}",
                settings.xkb_layout, settings.xkb_variant
            );
        }
        let rate = if settings.repeat && settings.repeat_interval_ms > 0 {
            (1000 / settings.repeat_interval_ms).max(1) as i32
        } else {
            0
        };
        keyboard.change_repeat_info(rate, settings.repeat_delay_ms as i32);
        self.repeat = (rate, settings.repeat_delay_ms as i32);
    }

    /// The next input source (GNOME's Super+Space).
    fn next_input_source(&mut self, state: &mut State) {
        self.switch_input_source(state, false);
    }

    /// The next (or previous) input source, as the shell's rebindable
    /// `switch-input-source` keys ask.
    pub fn switch_input_source(&mut self, state: &mut State, backward: bool) {
        if let Some(keyboard) = self.keyboard.clone() {
            keyboard.with_xkb_state(state, |mut context| {
                if backward {
                    context.cycle_prev_layout();
                } else {
                    context.cycle_next_layout();
                }
            });
        }
    }

    /// Whether a shell has taken GNOME's window-manager keys
    /// (org.gnome.desktop.wm.keybindings) by grabbing accelerators: the
    /// compositor's built-in defaults for them then stand aside, so
    /// rebound keys win and unbound ones reach clients.
    fn shell_owns_wm_keys(&self) -> bool {
        !self.accelerators.is_empty()
    }

    /// Active layout and repeat, for the state file.
    pub fn keyboard_summary(&self, state: &mut State) -> serde_json::Value {
        let Some(keyboard) = self.keyboard.clone() else {
            return serde_json::Value::Null;
        };
        let (layout, layouts) = keyboard.with_xkb_state(state, |context| {
            let xkb = context.xkb().lock().expect("xkb lock");
            let names: Vec<String> = xkb
                .layouts()
                .map(|l| xkb.layout_name(l).to_owned())
                .collect();
            (xkb.layout_name(xkb.active_layout()).to_owned(), names)
        });
        serde_json::json!({
            "layout": layout,
            "layouts": layouts,
            "repeat_rate": self.repeat.0,
            "repeat_delay": self.repeat.1,
        })
    }

    /// Dispatch one backend input event into focus and delivery.
    /// Super+PageUp/PageDown switches workspace (with Shift: moves the
    /// focused window and follows it); those presses are consumed, all
    /// other keys — modifiers included — still reach clients.
    /// Alt-Tab drives the shell switcher: Tab events while Alt is held
    /// never reach clients (taps queue `Step`, Shift reverses,
    /// releases are swallowed), Alt release queues `Commit`, Escape
    /// queues `Cancel`; the Alt/Escape releases still reach clients.
    /// Alt+F4 asks the focused client to close (cancelling an open
    /// switcher with it); its press is consumed too.
    pub fn on_input(&mut self, state: &mut State, input: ManagerInput) {
        match input {
            ManagerInput::Key {
                keycode,
                pressed,
                time,
            } => {
                self.track_workspace_modifiers(keycode, pressed);
                self.track_switcher_modifiers(keycode, pressed);
                // Exclusive shell overlays own their navigation chords. Normal
                // workspace/window/switcher shortcuts must not steal Alt arrows.
                // Explicit popup-mode system accelerators still run below.
                if crate::layer::exclusive_popup_keyboard_layer(state).is_some() {
                    if !pressed && self.switcher_swallowed.contains(&keycode) {
                        self.switcher_swallowed.retain(|k| *k != keycode);
                    } else {
                        self.keyboard_key(state, keycode, pressed, time);
                    }
                    return;
                }
                if !self.switcher_open && state.shortcuts_inhibited() {
                    self.inhibited_key(state, keycode, pressed, time);
                    return;
                }
                let modifier = self.is_alt(keycode)
                    || matches!(
                        keycode,
                        SUPER_LEFT_KEYCODE
                            | SUPER_RIGHT_KEYCODE
                            | SHIFT_LEFT_KEYCODE
                            | SHIFT_RIGHT_KEYCODE
                            | CTRL_LEFT_KEYCODE
                            | CTRL_RIGHT_KEYCODE
                    );
                if !pressed && self.switcher_swallowed.contains(&keycode) {
                    // The switcher took the press: the release too.
                    self.switcher_swallowed.retain(|k| *k != keycode);
                } else if let Some((kind, opener)) = self.switcher_chord(state, keycode, pressed) {
                    // One of the shell's switcher chords. The press and
                    // its release stay invisible to apps.
                    self.switcher_swallowed.push(keycode);
                    use roost_shell_control::SwitcherKeyKind as K;
                    if matches!(
                        kind,
                        K::CycleWindows
                            | K::CycleWindowsBackward
                            | K::CycleGroup
                            | K::CycleGroupBackward
                    ) {
                        if self.switcher_open {
                            self.push_switcher(SwitcherAction::Cancel);
                        }
                        self.switcher_open = false;
                        self.push_switcher(SwitcherAction::Cycle {
                            forward: matches!(kind, K::CycleWindows | K::CycleGroup),
                            group: matches!(kind, K::CycleGroup | K::CycleGroupBackward),
                        });
                        return;
                    }
                    let reopen = !self.switcher_open;
                    if reopen {
                        self.switcher_opener = opener;
                    }
                    self.switcher_open = true;
                    self.push_switcher(match kind {
                        K::Applications => SwitcherAction::Step { forward: true },
                        K::ApplicationsBackward => SwitcherAction::Step { forward: false },
                        K::Group => SwitcherAction::StepWindow { forward: true },
                        K::GroupBackward => SwitcherAction::StepWindow { forward: false },
                        K::Windows => SwitcherAction::StepAllWindows { forward: true },
                        K::WindowsBackward => SwitcherAction::StepAllWindows { forward: false },
                        _ => unreachable!("cycle keys returned above"),
                    });
                    if reopen && opener == 0 {
                        // No modifier to hold it open: GNOME picks at once.
                        self.switcher_open = false;
                        self.push_switcher(SwitcherAction::Commit);
                    }
                } else if pressed
                    && keycode == GRAVE_KEYCODE
                    && self.switcher_keys.is_none()
                    && (self.switcher_open || self.alt_held || self.super_held)
                {
                    // GNOME's switch-group (Alt+Above_Tab): the selected
                    // app's windows, opening the switcher when closed.
                    if !self.switcher_open {
                        self.switcher_opener = self.builtin_opener();
                    }
                    self.switcher_open = true;
                    self.switcher_swallowed.push(keycode);
                    self.push_switcher(SwitcherAction::StepWindow {
                        forward: !self.shift_held,
                    });
                } else if pressed
                    && self.switcher_open
                    && !modifier
                    && (keycode != TAB_KEYCODE || self.switcher_keys.is_some())
                    && keycode != ESCAPE_KEYCODE
                {
                    // GNOME's switcher holds the keyboard: arrows, Q, W
                    // and F4 act on it, every other key goes nowhere.
                    self.switcher_swallowed.push(keycode);
                    if let Some(keysym) = switcher_keysym(keycode) {
                        self.push_switcher(SwitcherAction::Key { keysym });
                    }
                } else if keycode == SPACE_KEYCODE && self.super_held && !self.shell_owns_wm_keys()
                {
                    // GNOME's next input source; press and release stay
                    // with the compositor.
                    if pressed {
                        self.next_input_source(state);
                    }
                } else if keycode == TAB_KEYCODE
                    && self.switcher_keys.is_none()
                    && (self.alt_held || self.super_held)
                {
                    // Alt+Tab and Super+Tab (GNOME's switch-applications
                    // defaults). The whole chord stays invisible to apps:
                    // taps queue steps, releases are swallowed (an app
                    // that never saw the press must not see the release
                    // either).
                    if pressed {
                        if !self.switcher_open {
                            // Released, this modifier commits.
                            self.switcher_opener = self.builtin_opener();
                        }
                        self.switcher_open = true;
                        self.push_switcher(SwitcherAction::Step {
                            forward: !self.shift_held,
                        });
                    }
                } else if keycode == F4_KEYCODE && self.alt_held && !self.shell_owns_wm_keys() {
                    // Alt+F4 closes the focused window politely. An open
                    // switcher cancels with it: committing onto a window
                    // the user just closed would surprise. Press and
                    // release both stay invisible: a client that never
                    // saw the press must not see the release either.
                    if pressed {
                        if self.switcher_open {
                            self.switcher_open = false;
                            self.push_switcher(SwitcherAction::Cancel);
                        }
                        self.close_focused();
                    }
                } else if pressed && keycode == ESCAPE_KEYCODE && self.switcher_open {
                    self.switcher_open = false;
                    self.push_switcher(SwitcherAction::Cancel);
                } else {
                    if !pressed
                        && self.switcher_open
                        && modifier
                        && self.held_mods() & self.switcher_opener == 0
                    {
                        self.switcher_open = false;
                        self.push_switcher(SwitcherAction::Commit);
                    }
                    self.on_workspace_key(state, keycode, pressed, time);
                }
            }
            ManagerInput::Motion { pos, time } => self.pointer_motion(state, pos, time),
            ManagerInput::Button {
                button,
                pressed,
                time,
            } => self.pointer_button(state, button, pressed, time),
            ManagerInput::Axis {
                horizontal,
                vertical,
                time,
            } => {
                // Wheel scrolls the strip in scroll mode (focus stays
                // on the same window); everywhere else it scrolls the
                // client under the pointer, as in GNOME.
                if !self.scroll_strip(state, horizontal, vertical) {
                    self.pointer_axis(state, horizontal, vertical, time);
                }
            }
            ManagerInput::CornerPressure { .. } => {}
            ManagerInput::RelativeMotion {
                delta,
                delta_unaccel,
                utime,
            } => self.relative_motion(state, delta, delta_unaccel, utime),
            ManagerInput::SwipeBegin { fingers, time } => {
                if let Some(pointer) = self.pointer.clone() {
                    pointer.gesture_swipe_begin(
                        state,
                        &smithay::input::pointer::GestureSwipeBeginEvent {
                            serial: SERIAL_COUNTER.next_serial(),
                            time,
                            fingers,
                        },
                    );
                    pointer.frame(state);
                }
            }
            ManagerInput::SwipeUpdate { delta, time } => {
                if let Some(pointer) = self.pointer.clone() {
                    pointer.gesture_swipe_update(
                        state,
                        &smithay::input::pointer::GestureSwipeUpdateEvent { time, delta },
                    );
                    pointer.frame(state);
                }
            }
            ManagerInput::SwipeEnd { cancelled, time } => {
                if let Some(pointer) = self.pointer.clone() {
                    pointer.gesture_swipe_end(
                        state,
                        &smithay::input::pointer::GestureSwipeEndEvent {
                            serial: SERIAL_COUNTER.next_serial(),
                            time,
                            cancelled,
                        },
                    );
                    pointer.frame(state);
                }
            }
        }
    }

    /// A key while the focused window inhibits shortcuts
    /// (keyboard-shortcuts-inhibit): every chord reaches the window,
    /// except Super+Escape, Mutter's restore-shortcuts, which hands the
    /// shortcuts back (press and release stay with the compositor).
    fn inhibited_key(&mut self, state: &mut State, keycode: u32, pressed: bool, time: u32) {
        if !pressed && self.switcher_swallowed.contains(&keycode) {
            self.switcher_swallowed.retain(|k| *k != keycode);
            return;
        }
        if pressed && keycode == ESCAPE_KEYCODE && self.super_held && state.restore_shortcuts() {
            eprintln!("roost-compositor: shortcuts restored");
            self.switcher_swallowed.push(keycode);
            return;
        }
        self.keyboard_key(state, keycode, pressed, time);
    }

    /// Pointer warps clients asked for (pointer-warp): honoured when
    /// the surface still has pointer focus from the enter they name,
    /// and lands inside a window or layer surface Roost placed.
    fn drain_pointer_warps(&mut self, state: &mut State) {
        for (surface, local, serial) in state.take_pointer_warps() {
            let Some(pointer) = self.pointer.clone() else {
                continue;
            };
            if pointer.current_focus().as_ref() != Some(&surface)
                || pointer.last_enter().map(u32::from) != Some(serial)
            {
                continue;
            }
            let origin = self
                .windows
                .values()
                .find(|w| w.surface.wl_surface().is_some_and(|s| *s == surface))
                .map(|w| crate::popup::surface_origin(&surface, w.geometry.loc))
                .or_else(|| {
                    crate::layer::layer_layout(state)
                        .into_iter()
                        .find(|(s, _, _)| *s == surface)
                        .map(|(_, (x, y), _)| Point::from((x, y)))
                });
            let Some(origin) = origin else {
                continue;
            };
            let pos = origin.to_f64() + local;
            let time = (crate::state::system_millis() & u64::from(u32::MAX)) as u32;
            self.pointer_motion(state, pos, time);
        }
    }

    /// Raw motion to the client under the pointer (relative-pointer).
    fn relative_motion(
        &mut self,
        state: &mut State,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        utime: u64,
    ) {
        let Some(pointer) = self.pointer.clone() else {
            return;
        };
        let focus = pointer.current_focus().map(|surface| {
            let origin = self.surface_origin_of(state, &surface);
            (surface, origin)
        });
        pointer.relative_motion(
            state,
            focus,
            &smithay::input::pointer::RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime,
            },
        );
        pointer.frame(state);
    }

    /// Resolve the compositor's actual placed geometry, including layers
    /// on outputs whose logical origin is not the global origin.
    fn constrained_surface_geometry(
        &self,
        state: &State,
        surface: &WlSurface,
    ) -> Option<(Point<f64, Logical>, Rectangle<i32, Logical>)> {
        if let Some(window) = self
            .windows
            .values()
            .find(|window| window.surface.wl_surface().as_deref() == Some(surface))
        {
            let origin = crate::popup::surface_origin(surface, window.geometry.loc);
            return Some((
                origin.to_f64(),
                Rectangle::new(window.geometry.loc - origin, window.geometry.size),
            ));
        }
        if let Some((_, (x, y), _)) = crate::layer::layer_layout(state)
            .into_iter()
            .find(|(placed, _, _)| placed == surface)
        {
            let size = state
                .layer_shell_state
                .layer_surfaces()
                .find(|layer| layer.wl_surface() == surface)
                .and_then(|layer| layer.current_state().size)?;
            return Some((
                (f64::from(x), f64::from(y)).into(),
                Rectangle::from_size(size),
            ));
        }
        self.placed_popups(state)
            .into_iter()
            .find(|popup| &popup.surface == surface)
            .map(|popup| {
                (
                    popup.origin.to_f64(),
                    Rectangle::new(popup.rect.loc - popup.origin, popup.rect.size),
                )
            })
    }

    fn surface_origin_of(&self, state: &State, surface: &WlSurface) -> Point<f64, Logical> {
        self.constrained_surface_geometry(state, surface)
            .map(|(origin, _)| origin)
            .unwrap_or_default()
    }

    fn effective_constraint_region(
        &self,
        state: &State,
        surface: &WlSurface,
    ) -> Option<(
        Point<f64, Logical>,
        crate::constraint_motion::EffectiveRegion,
    )> {
        let (origin, extent) = self.constrained_surface_geometry(state, surface)?;
        let (input, over_budget) = smithay::wayland::compositor::with_states(surface, |states| {
            let mut attributes = states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>();
            let current = attributes.current();
            let total = 1usize.saturating_add(
                current
                    .input_region
                    .as_ref()
                    .map_or(0, |region| region.rects.len()),
            );
            let over_budget = total > crate::constraint_motion::MAX_RECTANGLES;
            (
                if over_budget {
                    None
                } else {
                    current.input_region.clone()
                },
                over_budget,
            )
        });
        Some((
            origin,
            crate::constraint_motion::EffectiveRegion {
                extent,
                input,
                over_budget,
                constraint: None,
            },
        ))
    }

    /// Active constraints and eligible pending constraints own the next
    /// motion before native barriers. Ineligible persistent registrations
    /// can later reactivate; they do not permanently swallow corner input.
    pub fn pointer_constraint_owns_motion(&self, state: &State) -> bool {
        use smithay::wayland::pointer_constraints::with_pointer_constraint;
        let Some(pointer) = self.pointer.as_ref() else {
            return false;
        };
        let Some(surface) = pointer.current_focus() else {
            return false;
        };
        if !with_pointer_constraint(&surface, pointer, |constraint| constraint.is_some()) {
            return false;
        }
        let mut owns_motion = false;
        // with_pointer_constraint also holds the surface-state mutex. Take
        // geometry/input snapshots first; its closure must not re-enter it.
        let snapshot = self.effective_constraint_region(state, &surface);
        with_pointer_constraint(&surface, pointer, |constraint| {
            if let Some(constraint) = constraint {
                owns_motion = constraint.is_active()
                    || snapshot.is_none_or(|(origin, region)| {
                        let region = region.with_constraint(constraint.region());
                        !region.bounded() || region.contains(self.pointer_pos - origin)
                    });
            }
        });
        owns_motion
    }

    /// Apply actual committed constraint/input regions in surface-local
    /// space. Region changes may explicitly deactivate rather than warp;
    /// no synthetic relative motion is introduced.
    fn constrain(&self, state: &State, pos: Point<f64, Logical>) -> Option<Point<f64, Logical>> {
        use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};
        let pointer = self.pointer.as_ref()?;
        let Some(surface) = pointer.current_focus() else {
            return Some(pos);
        };
        if !with_pointer_constraint(&surface, pointer, |constraint| constraint.is_some()) {
            return Some(pos);
        }
        let mut position = Some(pos);
        let snapshot = self.effective_constraint_region(state, &surface);
        with_pointer_constraint(&surface, pointer, |constraint| {
            let Some(constraint) = constraint else { return };
            let Some((origin, region)) = snapshot else {
                // A constraint on unresolved geometry cannot broaden motion.
                position = None;
                return;
            };
            let region = region.with_constraint(constraint.region());
            if !region.bounded() {
                position = None;
                return;
            }
            let current = self.pointer_pos - origin;
            if !region.contains(current) {
                if constraint.is_active() {
                    constraint.deactivate();
                }
                return;
            }
            if !constraint.is_active() {
                constraint.activate();
            }
            position = match &*constraint {
                PointerConstraint::Locked(_) => None,
                PointerConstraint::Confined(_) => {
                    Some(region.confine_global(origin, self.pointer_pos, pos))
                }
            };
        });
        position
    }

    /// Queued switcher drive events, drained by the runtime into the
    /// hub broadcast after each input event.
    pub fn take_switcher_queue(&mut self) -> Vec<SwitcherAction> {
        std::mem::take(&mut self.switcher_queue)
    }

    /// Super-chord window actions (002 workspaces and window actions).
    /// PageUp/PageDown switches workspace (Shift: moves the focused
    /// window and follows it); arrows drive managed layouts on the
    /// focused window (Up maximizes, Down restores, Left/Right tiles
    /// that half, repeat toggles back). Those presses are consumed,
    /// everything else — modifiers included — still reaches clients.
    fn on_workspace_key(&mut self, state: &mut State, keycode: u32, pressed: bool, time: u32) {
        let builtin = !self.shell_owns_wm_keys();
        if builtin && pressed && self.super_held && !self.shift_held && keycode == H_KEYCODE {
            // GNOME's minimize binding: Super+H hides the focused window.
            if let Some(id) = self.model.focused() {
                self.minimize(state, id);
            }
        } else if pressed && self.super_held && self.shift_held && keycode == T_KEYCODE {
            // Whole-session mode toggle (scrollable-tiling spec):
            // Super+Shift+T flips floating/strip and re-lays out every
            // window in place. Consumed like the other Super chords;
            // the release still reaches clients.
            let scroll = self.mode != SessionMode::Scroll;
            self.set_scroll(state, scroll);
        } else if pressed && self.super_held && keycode == R_KEYCODE {
            // Strip preset cycling: Super+R steps forward,
            // Super+Shift+R backward through 1/3–1/2–2/3. Scroll-only:
            // gnome mode forwards the press untouched (no R binding).
            if self.mode == SessionMode::Scroll {
                let direction = if self.shift_held { -1 } else { 1 };
                self.cycle_preset(state, direction);
            } else {
                self.keyboard_key(state, keycode, pressed, time);
            }
        } else if builtin
            && pressed
            && self.super_held
            && (keycode == PAGE_UP_KEYCODE || keycode == PAGE_DOWN_KEYCODE)
        {
            let delta = if keycode == PAGE_UP_KEYCODE { -1 } else { 1 };
            let switched = if self.shift_held {
                self.move_focused_relative(state, delta)
            } else {
                self.switch_relative(state, delta)
            };
            // Greppable end-to-end signal for the CI journey: the
            // switch happened (empty workspaces look identical on
            // screen, so pixels alone cannot prove it).
            if switched {
                eprintln!(
                    "roost-compositor: workspace now {}",
                    self.model.active_workspace()
                );
                // GNOME's switcher popup, outside the overview.
                if !self.overview_open {
                    self.workspace_popups.push(self.workspace_popup());
                }
            }
        } else if pressed
            && self.super_held
            && Self::is_arrow(keycode)
            && (builtin || self.mode == SessionMode::Scroll)
        {
            if self.mode == SessionMode::Scroll
                && (keycode == ARROW_LEFT_KEYCODE || keycode == ARROW_RIGHT_KEYCODE)
            {
                // Scroll-mode strip motion (niri shape): arrows move
                // focus along the strip, Shift+arrows move the focused
                // window one slot. Up/Down keep their gnome meaning;
                // forced membership converts the result to columns.
                // Consumed in all cases, like the gnome chords.
                let delta = if keycode == ARROW_LEFT_KEYCODE { -1 } else { 1 };
                if self.shift_held {
                    self.move_focused_in_strip(state, delta);
                } else {
                    self.focus_neighbor_in_strip(state, delta);
                }
            } else if let Some(id) = self.model.focused() {
                match keycode {
                    ARROW_UP_KEYCODE => {
                        self.set_maximized(state, id, true);
                    }
                    ARROW_DOWN_KEYCODE => {
                        // Forced strip membership: Down never restores
                        // floating mid-scroll; re-resolve the column.
                        if self.mode == SessionMode::Scroll {
                            self.apply_layout(state, id, WindowLayout::Strip);
                        } else {
                            self.restore_window(id);
                        }
                    }
                    ARROW_LEFT_KEYCODE => {
                        self.toggle_tiled(state, id, TileSide::Left);
                    }
                    ARROW_RIGHT_KEYCODE => {
                        self.toggle_tiled(state, id, TileSide::Right);
                    }
                    _ => {}
                }
            }
        } else {
            self.keyboard_key(state, keycode, pressed, time);
        }
    }

    /// Whether `keycode` is a layout-chord arrow.
    fn is_arrow(keycode: u32) -> bool {
        matches!(
            keycode,
            ARROW_UP_KEYCODE | ARROW_DOWN_KEYCODE | ARROW_LEFT_KEYCODE | ARROW_RIGHT_KEYCODE
        )
    }

    /// Tile a side, toggling back to floating on repeat (GNOME shape).
    fn toggle_tiled(&mut self, state: &mut State, id: u64, side: TileSide) -> bool {
        if self.window_layout(id) == Some(WindowLayout::Tiled(side)) {
            self.restore_window(id)
        } else {
            self.set_tiled(state, id, side)
        }
    }

    /// Whether `keycode` is either Alt side.
    fn is_alt(&self, keycode: u32) -> bool {
        keycode == ALT_LEFT_KEYCODE || keycode == ALT_RIGHT_KEYCODE
    }

    /// Track Alt hold state for the switcher. The modifier events
    /// themselves still forward to clients.
    fn track_switcher_modifiers(&mut self, keycode: u32, pressed: bool) {
        if self.is_alt(keycode) {
            self.alt_held = pressed;
        } else if keycode == CTRL_LEFT_KEYCODE || keycode == CTRL_RIGHT_KEYCODE {
            self.ctrl_held = pressed;
        }
    }

    /// The modifiers held now, as `MOD_*` bits.
    fn held_mods(&self) -> u32 {
        use roost_shell_control::{MOD_ALT, MOD_CTRL, MOD_LOGO, MOD_SHIFT};
        [
            (self.shift_held, MOD_SHIFT),
            (self.ctrl_held, MOD_CTRL),
            (self.alt_held, MOD_ALT),
            (self.super_held, MOD_LOGO),
        ]
        .into_iter()
        .filter(|(held, _)| *held)
        .fold(0, |bits, (_, bit)| bits | bit)
    }

    /// The built-in chords' opener: Alt when held, else Super.
    fn builtin_opener(&self) -> u32 {
        if self.alt_held {
            roost_shell_control::MOD_ALT
        } else {
            roost_shell_control::MOD_LOGO
        }
    }

    /// The shell's switcher chord this press makes, with the modifiers
    /// that hold the switcher open. Matched as Mutter matches: the key's
    /// base-level keysym in the active layout and the exact modifiers;
    /// Shift on a forward chord steps backward, as in GNOME's popup.
    fn switcher_chord(
        &self,
        state: &mut State,
        keycode: u32,
        pressed: bool,
    ) -> Option<(roost_shell_control::SwitcherKeyKind, u32)> {
        use roost_shell_control::{SwitcherKeyKind as K, KEYSYM_ABOVE_TAB, MOD_SHIFT};
        let keys = self
            .switcher_keys
            .as_ref()
            .filter(|k| pressed && !k.is_empty())?;
        let sym = self.base_keysym(state, keycode);
        let mods = self.held_mods();
        let key = keys.iter().find(|k| {
            let same = if k.keysym == KEYSYM_ABOVE_TAB {
                keycode == GRAVE_KEYCODE
            } else {
                sym == Some(k.keysym)
            };
            same && (k.mods == mods || k.mods | MOD_SHIFT == mods)
        })?;
        let flip = mods & MOD_SHIFT != 0 && key.mods & MOD_SHIFT == 0;
        let kind = match (key.kind, flip) {
            (K::Applications, true) => K::ApplicationsBackward,
            (K::ApplicationsBackward, true) => K::Applications,
            (K::Group, true) => K::GroupBackward,
            (K::GroupBackward, true) => K::Group,
            (K::Windows, true) => K::WindowsBackward,
            (K::WindowsBackward, true) => K::Windows,
            (K::CycleWindows, true) => K::CycleWindowsBackward,
            (K::CycleWindowsBackward, true) => K::CycleWindows,
            (K::CycleGroup, true) => K::CycleGroupBackward,
            (K::CycleGroupBackward, true) => K::CycleGroup,
            (kind, false) => kind,
        };
        Some((kind, key.mods & !MOD_SHIFT))
    }

    /// The keysym `keycode` types at the base level of the active layout.
    fn base_keysym(&self, state: &mut State, keycode: u32) -> Option<u32> {
        let keyboard = self.keyboard.clone()?;
        keyboard.with_xkb_state(state, |context| {
            let xkb = context.xkb().lock().ok()?;
            let layout = xkb.active_layout().0;
            // SAFETY: the keymap is only borrowed while the lock is held.
            let keymap = unsafe { xkb.keymap() };
            keymap
                .key_get_syms_by_level((keycode + XKB_X11_OFFSET).into(), layout, 0)
                .first()
                .map(|sym| sym.raw())
        })
    }

    /// Queue one switcher drive event with a greppable trail for the
    /// CI journey (an empty switcher renders nothing, so pixels alone
    /// cannot prove the drive arrived).
    fn push_switcher(&mut self, action: SwitcherAction) {
        eprintln!("roost-compositor: switcher {action:?}");
        self.switcher_queue.push(action);
    }
}

/// Mutter's `find_next_cascade` (place.c): starting where the window
/// would go alone, each window whose corner sits within 10px of the
/// cascade point pushes it one step down the diagonal, the windows taken
/// north-west first; so a new window takes the first free slot of the
/// cascade. A cascade running off the work area starts over at its
/// top-left, 50px further right each time.
fn next_cascade(
    start: Point<i32, Logical>,
    mut others: Vec<Point<i32, Logical>>,
    size: Size<i32, Logical>,
    work: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    const THRESHOLD: i32 = 10;
    const CASCADE_INTERVAL: i32 = 50;
    others.sort_by_key(|p| {
        let (x, y) = (i64::from(p.x), i64::from(p.y));
        x * x + y * y
    });
    let (mut x, mut y) = (start.x, start.y);
    let mut stage = 0;
    let mut i = 0;
    while i < others.len() {
        let w = others[i];
        if (w.x - x).abs() < THRESHOLD && (w.y - y).abs() < THRESHOLD {
            x = w.x + CASCADE_STEP;
            y = w.y + CASCADE_STEP;
            if x + size.w > work.loc.x + work.size.w || y + size.h > work.loc.y + work.size.h {
                stage += 1;
                x = work.loc.x.max(0) + CASCADE_INTERVAL * stage;
                y = work.loc.y.max(0);
                if x + size.w < work.loc.x + work.size.w {
                    i = 0;
                    continue;
                }
                x = work.loc.x.max(0);
                break;
            }
        }
        i += 1;
    }
    (x, y).into()
}

/// Translate a winit backend [`InputEvent`] into [`ManagerInput`].
/// Returns `None` for device add/remove and other unconsumed kinds.
/// Button code synthesized for touch contacts: the shell gates tile,
/// dock, and banner presses on left-click, and clients expect balanced
/// press/release pairs, so a tap must arrive as a left press to act.
const TOUCH_BUTTON: u32 = 0x110;

/// One touch contact as pointer inputs: move to the contact, then press.
/// Motion must precede the press because press handlers act at the last
/// motion position, not at any position the button event carries.
/// Pure so unit tests pin the pairing without backend event types.
fn touch_press(pos: (f64, f64), time: u32) -> [ManagerInput; 2] {
    [
        ManagerInput::Motion {
            pos: pos.into(),
            time,
        },
        ManagerInput::Button {
            button: TOUCH_BUTTON,
            pressed: true,
            time,
        },
    ]
}

/// End of one touch contact: release the synthesized press so no
/// button sticks down. Cancel releases too: a cancelled contact must
/// never leave a press behind either.
fn touch_release(time: u32) -> ManagerInput {
    ManagerInput::Button {
        button: TOUCH_BUTTON,
        pressed: false,
        time,
    }
}

/// Pixels per wheel notch, as Mutter scrolls.
pub const WHEEL_STEP_PX: f64 = 15.0;

/// Whether an axis amount is a whole number of v120 wheel units.
pub fn is_v120_wheel(amount: f64) -> bool {
    amount.fract() == 0.0 && (amount as i64) % 120 == 0
}

/// Translate one backend event into manager inputs. Touch contacts
/// emulate a left-button pointer (down moves then presses, motion
/// moves, up/cancel releases) so touchscreens act instead of dropping
/// silently. Multi-touch overlaps may interleave presses — there is no
/// per-slot tracking here — but every release still balances.
///
/// Generic over the backend (nested winit or hardware libinput): every
/// backend reports XKB keycodes (evdev + 8) and absolute positions are
/// scaled into `area`, the logical size the event's device maps onto
/// (the nested window, or the primary output on hardware). Relative
/// pointer motion is backend-owned and yields nothing here.
pub fn translate_input<B: InputBackend>(
    event: InputEvent<B>,
    area: Size<i32, Logical>,
) -> Vec<ManagerInput> {
    match event {
        InputEvent::Keyboard { event } => vec![ManagerInput::Key {
            // The winit backend reports X11 keycodes (kernel evdev
            // number plus 8); normalize back to evdev so trigger and
            // overlay tables written in evdev keycodes match on every
            // backend. X keycodes below 8 cannot occur; saturate.
            keycode: u32::from(event.key_code()).saturating_sub(XKB_X11_OFFSET),
            pressed: event.state() == KeyState::Pressed,
            time: (event.time() / 1000) as u32,
        }],
        InputEvent::PointerMotionAbsolute { event } => {
            let pos = event.position_transformed(area);
            vec![ManagerInput::Motion {
                pos: (pos.x, pos.y).into(),
                time: (event.time() / 1000) as u32,
            }]
        }
        InputEvent::PointerButton { event } => vec![ManagerInput::Button {
            button: event.button_code(),
            pressed: event.state() == ButtonState::Pressed,
            time: (event.time() / 1000) as u32,
        }],
        InputEvent::PointerAxis { event } => {
            let axis_amount = |axis: Axis| {
                event
                    .amount_v120(axis)
                    .or_else(|| event.amount(axis))
                    .unwrap_or(0.0)
            };
            vec![ManagerInput::Axis {
                horizontal: axis_amount(Axis::Horizontal),
                vertical: axis_amount(Axis::Vertical),
                time: (event.time() / 1000) as u32,
            }]
        }
        InputEvent::TouchDown { event } => {
            let pos = event.position_transformed(area);
            touch_press((pos.x, pos.y), (event.time() / 1000) as u32).into()
        }
        InputEvent::TouchMotion { event } => {
            let pos = event.position_transformed(area);
            vec![ManagerInput::Motion {
                pos: (pos.x, pos.y).into(),
                time: (event.time() / 1000) as u32,
            }]
        }
        InputEvent::TouchUp { event } => vec![touch_release((event.time() / 1000) as u32)],
        InputEvent::TouchCancel { event } => vec![touch_release((event.time() / 1000) as u32)],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evdev_switcher_wire_mapping_uses_shared_keys_and_refuses_unmapped_input() {
        for (code, keysym) in [
            (ARROW_LEFT_KEYCODE, 0xff51),
            (ARROW_UP_KEYCODE, 0xff52),
            (ARROW_RIGHT_KEYCODE, 0xff53),
            (ARROW_DOWN_KEYCODE, 0xff54),
            (F4_KEYCODE, 0xffc1),
            (Q_KEYCODE, 0x71),
            (W_KEYCODE, 0x77),
        ] {
            assert_eq!(switcher_keysym(code), Some(keysym));
        }
        for code in [0, TAB_KEYCODE, GRAVE_KEYCODE, CTRL_LEFT_KEYCODE, u32::MAX] {
            assert_eq!(switcher_keysym(code), None);
        }
    }

    #[test]
    fn new_windows_take_the_first_free_cascade_slot_like_mutter() {
        let work = Rectangle::new((0, 32).into(), (1280, 768).into());
        let size: Size<i32, Logical> = (640, 420).into();
        let c: Point<i32, Logical> = (320, 206).into();
        let step = |n: i32| c + Point::from((50 * n, 50 * n));
        // Mapped in order: each cascades from the last.
        assert_eq!(next_cascade(c, vec![c], size, work), step(1));
        assert_eq!(next_cascade(c, vec![step(1), c], size, work), step(2));
        // The middle one gone: its slot is the first free one.
        assert_eq!(next_cascade(c, vec![step(2), c], size, work), step(1));
        // Off the work area: a new cascade from the top-left, 50px right.
        let full = vec![c, step(1), step(2), step(3), step(4), step(5), step(6)];
        assert_eq!(next_cascade(c, full, size, work), (50, 32).into());
    }

    fn key(keycode: u32, pressed: bool) -> ManagerInput {
        ManagerInput::Key {
            keycode,
            pressed,
            time: 0,
        }
    }

    fn motion(x: f64, y: f64) -> ManagerInput {
        ManagerInput::Motion {
            pos: (x, y).into(),
            time: 0,
        }
    }

    fn button(pressed: bool) -> ManagerInput {
        ManagerInput::Button {
            button: 0x110,
            pressed,
            time: 0,
        }
    }

    #[test]
    fn touch_press_moves_then_presses_left() {
        let [motion, press] = touch_press((1263.0, 16.0), 7);
        assert!(
            matches!(motion, ManagerInput::Motion { pos, time: 7 } if pos.x == 1263.0 && pos.y == 16.0),
            "tap must move first, presses act at the last motion position: {motion:?}"
        );
        assert!(
            matches!(
                press,
                ManagerInput::Button {
                    button: 0x110,
                    pressed: true,
                    time: 7
                }
            ),
            "tap must arrive as a left press or the shell ignores it: {press:?}"
        );
    }

    #[test]
    fn touch_release_balances_with_left_release() {
        assert!(
            matches!(
                touch_release(9),
                ManagerInput::Button {
                    button: 0x110,
                    pressed: false,
                    time: 9
                }
            ),
            "lift and cancel must release the synthesized press"
        );
    }

    #[test]
    fn lone_super_tap_toggles() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, true), false, (0.0, 100.0).into()),
            TriggerAction::None,
            "press alone arms without acting"
        );
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, false), false, (0.0, 100.0).into()),
            TriggerAction::Toggle
        );
        // Right Super taps too.
        assert_eq!(
            triggers.feed(&key(SUPER_RIGHT_KEYCODE, true), true, (0.0, 100.0).into()),
            TriggerAction::None
        );
        assert_eq!(
            triggers.feed(&key(SUPER_RIGHT_KEYCODE, false), true, (0.0, 100.0).into()),
            TriggerAction::Toggle
        );
    }

    #[test]
    fn super_combo_does_not_toggle() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, true), false, (0.0, 100.0).into()),
            TriggerAction::None
        );
        // Any other key in between cancels the tap (Super+T etc.).
        assert_eq!(
            triggers.feed(&key(20, true), false, (0.0, 100.0).into()),
            TriggerAction::None
        );
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, false), false, (0.0, 100.0).into()),
            TriggerAction::None,
            "release after a combo is not a tap"
        );
    }

    #[test]
    fn hot_corner_follows_gnomes_setting() {
        let mut triggers = TriggerState::default();
        triggers.set_hot_corner(false);
        assert_eq!(
            triggers.feed(&motion(2.0, 3.0), false, (0.0, 3.0).into()),
            TriggerAction::None,
            "enable-hot-corners off"
        );
        triggers.set_hot_corner(true);
        assert_eq!(
            triggers.feed(&motion(2.0, 3.0), false, (0.0, 3.0).into()),
            TriggerAction::Open
        );
    }

    #[test]
    fn hot_corner_opens_only_when_closed() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&motion(2.0, 3.0), false, (0.0, 3.0).into()),
            TriggerAction::Open
        );
        assert_eq!(
            triggers.feed(&motion(2.0, 3.0), true, (0.0, 3.0).into()),
            TriggerAction::None,
            "no re-open while already open"
        );
        assert_eq!(
            triggers.feed(&motion(400.0, 300.0), false, (0.0, 300.0).into()),
            TriggerAction::None
        );
    }

    #[test]
    fn activities_triggers_reject_outside_and_nonfinite_coordinates() {
        let mut triggers = TriggerState::default();
        for outside in [-1.0, f64::NEG_INFINITY, f64::INFINITY, f64::NAN] {
            for pos in [(outside, 2.0), (2.0, outside)] {
                assert_eq!(
                    triggers.feed(&motion(pos.0, pos.1), false, pos.into()),
                    TriggerAction::None,
                    "motion outside the corner: {pos:?}"
                );
                assert_eq!(
                    triggers.feed(&button(true), false, pos.into()),
                    TriggerAction::None,
                    "click outside the Activities strip: {pos:?}"
                );
            }
        }
        for pos in [(HOT_CORNER_PX, 0.0), (0.0, HOT_CORNER_PX)] {
            assert_eq!(
                triggers.feed(&motion(pos.0, pos.1), false, pos.into()),
                TriggerAction::None,
                "corner upper bound is exclusive"
            );
        }
        for pos in [(ACTIVITIES_WIDTH_PX, 0.0), (0.0, ACTIVITIES_STRIP_PX)] {
            assert_eq!(
                triggers.feed(&button(true), false, pos.into()),
                TriggerAction::None,
                "strip upper bound is exclusive"
            );
        }
        for pos in [(0.0, 0.0), (HOT_CORNER_PX - 0.5, HOT_CORNER_PX - 0.5)] {
            assert_eq!(
                triggers.feed(&motion(pos.0, pos.1), false, pos.into()),
                TriggerAction::Open,
                "valid corner remains active"
            );
        }
        assert_eq!(
            triggers.feed(
                &button(true),
                false,
                (ACTIVITIES_WIDTH_PX - 0.5, ACTIVITIES_STRIP_PX - 0.5).into(),
            ),
            TriggerAction::Toggle,
            "valid Activities click remains active"
        );
    }

    #[test]
    fn strip_click_toggles_and_disarms_super() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, true), false, (0.0, 100.0).into()),
            TriggerAction::None
        );
        // Click in the Activities strip: toggles, and the earlier Super
        // press must not linger as an armed tap.
        assert_eq!(
            triggers.feed(&button(true), false, (0.0, 10.0).into()),
            TriggerAction::Toggle
        );
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, false), false, (0.0, 100.0).into()),
            TriggerAction::None,
            "strip click consumed the armed Super"
        );
        // Clicks in the strip right of Activities (clock, indicators)
        // belong to the shell and never toggle.
        assert_eq!(
            triggers.feed(&button(true), false, (640.0, 10.0).into()),
            TriggerAction::None
        );
        // Clicks below the strip do nothing.
        assert_eq!(
            triggers.feed(&button(true), false, (0.0, 200.0).into()),
            TriggerAction::None
        );
        // Releases never toggle.
        assert_eq!(
            triggers.feed(&button(false), false, (0.0, 10.0).into()),
            TriggerAction::None
        );
    }

    // `x11_surfaces` feeds the frame-callback loop in
    // `Runtime::render`. Only the no-X11-window side is coverable
    // here: `X11Surface` needs a live X connection, so no headed
    // entry can be fabricated headlessly.
    #[cfg(feature = "xwayland")]
    #[test]
    fn x11_surfaces_empty_for_empty_manager() {
        let mut compositor = crate::TestCompositor::new();
        let manager = compositor.window_manager();
        assert_eq!(manager.x11_surfaces().len(), 0);
    }

    #[cfg(feature = "xwayland")]
    #[test]
    fn x11_surfaces_empty_after_reconcile_without_windows() {
        // A reconcile tick with no live surfaces maps nothing, so the
        // frame-callback input stays empty.
        let mut compositor = crate::TestCompositor::new();
        let mut manager = compositor.window_manager();
        manager.reconcile(&mut compositor.state);
        assert!(manager.x11_surfaces().is_empty());
    }
}

#[cfg(test)]
mod resize_tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn each_edge_moves_only_its_side() {
        let r = rect(100, 100, 400, 300);
        assert_eq!(resized(r, EDGE_RIGHT, 50, 0), rect(100, 100, 450, 300));
        assert_eq!(resized(r, EDGE_BOTTOM, 0, 20), rect(100, 100, 400, 320));
        assert_eq!(resized(r, EDGE_LEFT, 50, 0), rect(150, 100, 350, 300));
        assert_eq!(resized(r, EDGE_TOP, 0, -20), rect(100, 80, 400, 320));
        assert_eq!(
            resized(r, EDGE_TOP | EDGE_LEFT, 10, 10),
            rect(110, 110, 390, 290)
        );
    }

    #[test]
    fn resize_never_goes_below_the_minimum() {
        let r = rect(0, 0, 400, 300);
        let tiny = resized(r, EDGE_LEFT | EDGE_TOP, 1000, 1000);
        assert_eq!((tiny.size.w, tiny.size.h), MIN_WINDOW_SIZE);
        assert_eq!(
            tiny.loc,
            (400 - MIN_WINDOW_SIZE.0, 300 - MIN_WINDOW_SIZE.1).into()
        );
    }
}

/// The size a client committed for its window: its xdg window geometry,
/// else its buffer's size. `None` before the first buffer.
fn committed_size(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> Option<Size<i32, Logical>> {
    let buffer = smithay::backend::renderer::utils::with_renderer_surface_state(surface, |s| {
        s.surface_size()
    })
    .flatten()?;
    let geometry = smithay::wayland::compositor::with_states(surface, |states| {
        states
            .cached_state
            .get::<smithay::wayland::shell::xdg::SurfaceCachedState>()
            .current()
            .geometry
    });
    Some(geometry.map(|g| g.size).unwrap_or(buffer))
}

/// The control schema's modifier bits for xkb's current modifiers.
fn accelerator_mods(m: &smithay::input::keyboard::ModifiersState) -> u32 {
    let mut bits = 0;
    if m.shift {
        bits |= roost_shell_control::MOD_SHIFT;
    }
    if m.ctrl {
        bits |= roost_shell_control::MOD_CTRL;
    }
    if m.alt {
        bits |= roost_shell_control::MOD_ALT;
    }
    if m.logo {
        bits |= roost_shell_control::MOD_LOGO;
    }
    bits
}

/// GNOME normal/dialog/modal-dialog/utility eligibility. X11 modal dialogs
/// retain the Dialog window type; missing type defaults to Normal per EWMH.
#[cfg(feature = "xwayland")]
fn introspect_x11_type(kind: Option<WmWindowType>) -> bool {
    matches!(
        kind,
        None | Some(WmWindowType::Normal | WmWindowType::Dialog | WmWindowType::Utility)
    )
}

#[cfg(all(test, feature = "xwayland"))]
mod introspect_type_tests {
    use super::*;

    #[test]
    fn gnome_picker_excludes_auxiliary_x11_roles_but_keeps_dialogs() {
        for kind in [
            None,
            Some(WmWindowType::Normal),
            Some(WmWindowType::Dialog),
            Some(WmWindowType::Utility),
        ] {
            assert!(introspect_x11_type(kind), "eligible type {kind:?}");
        }
        for kind in [
            WmWindowType::Desktop,
            WmWindowType::Dock,
            WmWindowType::Combo,
            WmWindowType::Dnd,
            WmWindowType::DropdownMenu,
            WmWindowType::Menu,
            WmWindowType::Notification,
            WmWindowType::PopupMenu,
            WmWindowType::Splash,
            WmWindowType::Toolbar,
            WmWindowType::Tooltip,
        ] {
            assert!(!introspect_x11_type(Some(kind)), "auxiliary type {kind:?}");
        }
    }
}

/// Preserve dimensions absent from X11's ConfigureRequest value mask.
#[cfg(feature = "xwayland")]
fn requested_x11_size(
    current: Size<i32, Logical>,
    width: Option<u32>,
    height: Option<u32>,
) -> Size<i32, Logical> {
    let dimension = |value: Option<u32>, fallback| {
        value
            .and_then(|value| i32::try_from(value).ok())
            .filter(|value| *value > 0)
            .unwrap_or(fallback)
    };
    (dimension(width, current.w), dimension(height, current.h)).into()
}

#[cfg(all(test, feature = "xwayland"))]
#[test]
fn x11_size_requests_preserve_unspecified_and_invalid_dimensions() {
    let current = (640, 420).into();
    assert_eq!(
        requested_x11_size(current, Some(1100), None),
        (1100, 420).into()
    );
    assert_eq!(
        requested_x11_size(current, None, Some(700)),
        (640, 700).into()
    );
    assert_eq!(
        requested_x11_size(current, Some(0), Some(u32::MAX)),
        current
    );
    assert_eq!(
        requested_x11_size((1, 1).into(), Some(1100), Some(700)),
        (1100, 700).into()
    );
}
