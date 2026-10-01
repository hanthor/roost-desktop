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
use smithay::xwayland::X11Surface;
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
use roost_shell_control::SwitcherAction;

/// Default floating size for a newly mapped window.
const DEFAULT_WIDTH: i32 = 800;
const DEFAULT_HEIGHT: i32 = 600;
/// Cascade offset for each newly mapped window.
const CASCADE_STEP: i32 = 32;

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
    /// Super held (either side) for workspace keybindings.
    super_held: bool,
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
    /// Session window-management mode (scrollable-tiling spec).
    /// Gnome by default; Super+Shift+T flips the whole session.
    mode: SessionMode,
    /// Horizontal strip view offset in logical pixels (scroll mode).
    /// Zero on entering scroll; clamped to the strip overflow.
    strip_offset: f64,
    /// Last hub overview flag seen (set by the runtime each tick).
    overview_open: bool,
    /// Window focused before the overview parked keyboard focus.
    pre_overview_focus: Option<u64>,
    /// Overview surface keyboard focus is parked on, if parked.
    overview_held: Option<WlSurface>,
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
            swallowed_button: None,
            grab: None,
            mode: SessionMode::Gnome,
            strip_offset: 0.0,
            super_held: false,
            shift_held: false,
            alt_held: false,
            switcher_open: false,
            switcher_queue: Vec::new(),
            overview_open: false,
            pre_overview_focus: None,
            overview_held: None,
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
                    workspace: entry.workspace,
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
        let active = self.model.active_workspace();
        self.stacking
            .iter()
            .copied()
            .filter(|id| {
                self.model
                    .window(*id)
                    .is_some_and(|entry| entry.workspace == active)
            })
            .filter_map(|id| {
                self.windows
                    .get(&id)
                    .map(|w| (w.surface.clone(), w.geometry))
            })
            .collect()
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
    pub fn reconcile(&mut self, state: &mut State) {
        // Popups (#88): drop dead trees, and once the last grabbed popup
        // is gone hand keyboard focus back to the focused window.
        state.popups.cleanup();
        state.window_origins = self
            .visible_windows()
            .into_iter()
            .filter_map(|(w, g)| w.wl_surface().map(|s| (s.into_owned(), g.loc)))
            .collect();
        // Never pull focus out from under a live grab.
        if state.take_popup_refocus() && !state.popup_grab_active() {
            let focused = self.model.focused();
            self.apply_focus(state, focused);
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
        #[cfg(feature = "xwayland")]
        self.drain_x11_events(state);
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
        if stranded {
            self.focus_topmost(state, active);
        }
        // Client window-state requests drain last so a request that
        // arrived with a surface's first commits (e.g. initial
        // maximized) applies after the surface is mapped.
        self.drain_window_requests(state);
        #[cfg(feature = "xwayland")]
        self.sync_seat_focus(state);
        // Overview focus parks last so newly mapped windows never
        // hold focus past this tick while the overview is open.
        self.reconcile_overview_focus(state);
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
                    }
                    if let Some(previous) = self.model.focused() {
                        self.configure(previous, false);
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
    fn restore_pre_overview_focus(&mut self, state: &mut State) {
        self.overview_held = None;
        let restore = self
            .pre_overview_focus
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.stacking.last().copied());
        self.pre_overview_focus = None;
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
        let geometry = self.placement(state);
        self.windows.insert(
            id,
            ManagedWindow {
                surface: window,
                geometry,
                layout: WindowLayout::Floating,
                restore: None,
                preset: None,
            },
        );
        self.place_transient(surface, id);
        self.finish_map(state, id);
        id
    }

    /// Shared tail of every map path (native and X11): stacking slot,
    /// scroll-mode membership, initial configure, and focus.
    fn finish_map(&mut self, state: &mut State, id: u64) {
        self.stacking.push(id);
        // Forced strip membership: windows mapped mid-scroll join the
        // strip as columns. Dialogs keep their after-parent stacking
        // slot, so they land in the adjacent column — the strip is
        // their rule instead of floating centering.
        if self.mode == SessionMode::Scroll {
            self.apply_layout(state, id, WindowLayout::Strip);
        }
        self.configure(id, true);
        self.apply_focus(state, Some(id));
    }

    /// Where a new floating window goes. With a real output this follows
    /// GNOME's automatic placement: the first window on an empty
    /// workspace is centered in the work area, later ones cascade from
    /// the most recent by one step, kept inside the work area (never
    /// under the top bar). Without an output (headless tests) the plain
    /// cascade from the origin stands.
    fn placement(&mut self, state: &State) -> Rectangle<i32, Logical> {
        let work = Self::work_area(state);
        if work.size.w <= 0 || work.size.h <= 0 {
            return self.cascade_geometry();
        }
        let size: Size<i32, Logical> = (
            DEFAULT_WIDTH.min(work.size.w),
            DEFAULT_HEIGHT.min(work.size.h),
        )
            .into();
        let active = self.model.active_workspace();
        let last = self.stacking.iter().rev().find_map(|id| {
            (self.model.window(*id)?.workspace == active)
                .then(|| self.windows.get(id).map(|w| w.geometry.loc))
                .flatten()
        });
        let centered: Point<i32, Logical> = (
            work.loc.x + (work.size.w - size.w) / 2,
            work.loc.y + (work.size.h - size.h) / 2,
        )
            .into();
        let mut loc = match last {
            None => centered,
            Some(prev) => prev + Point::from((CASCADE_STEP, CASCADE_STEP)),
        };
        // Wrap back to the top-left of the work area when the cascade
        // would push the window off it.
        if loc.x + size.w > work.loc.x + work.size.w || loc.y + size.h > work.loc.y + work.size.h {
            loc = work.loc;
        }
        loc.x = loc.x.max(work.loc.x);
        loc.y = loc.y.max(work.loc.y);
        Rectangle::new(loc, size)
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
        let geometry = self.cascade_geometry();
        self.windows.insert(
            id,
            ManagedWindow {
                surface: window,
                geometry,
                layout: WindowLayout::Floating,
                restore: None,
                preset: None,
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
                X11ManagerEvent::ConfigureRequest { id, size } => {
                    let Some(model) = self.x11_index.get(&id).copied() else {
                        continue;
                    };
                    if let (Some((w, h)), Some(window)) = (size, self.windows.get_mut(&model)) {
                        if w > 0 && h > 0 {
                            window.geometry.size = (w as i32, h as i32).into();
                        }
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

    /// Re-assert seat focus when the model focus outruns it — the X11
    /// association case: a window mapped before its surface bound
    /// gains keyboard focus once the surface commits. Never fights
    /// the overview park (guarded out). (xwayland feature only.)
    #[cfg(feature = "xwayland")]
    fn sync_seat_focus(&mut self, state: &mut State) {
        if self.overview_open || self.overview_held.is_some() {
            return;
        }
        let Some(keyboard) = self.keyboard.clone() else {
            return;
        };
        let wanted = self.model.focused().and_then(|id| {
            self.windows
                .get(&id)
                .and_then(|window| window.surface.wl_surface())
                .map(|surface| surface.into_owned())
        });
        if keyboard.current_focus() != wanted {
            self.apply_focus(state, self.model.focused());
        }
    }

    /// Stack a transient directly above its parent and center it
    /// there, so dialogs open over (and with focus over) the window
    /// that spawned them. Parentless windows and orphans keep their
    /// cascaded geometry and stacking slot.
    fn place_transient(&mut self, surface: &ToplevelSurface, id: u64) {
        let Some(parent_wl) = read_parent(surface) else {
            return;
        };
        let Some(parent_id) = self.surface_index.get(&parent_wl).copied() else {
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
                let x = (parent_geo.loc.x + (parent_geo.size.w - size.w) / 2).max(0);
                let y = (parent_geo.loc.y + (parent_geo.size.h - size.h) / 2).max(0);
                window.geometry.loc = (x, y).into();
            }
        }
    }

    /// Remove one window from the model and the index. Focus falls back
    /// to the topmost remaining window, if any. In scroll mode the
    /// remaining columns close ranks behind it.
    fn unmap(&mut self, state: &mut State, id: u64) {
        if let Some(window) = self.windows.remove(&id) {
            eprintln!("roost-compositor: window {id} unmapped");
            match window.surface.underlying_surface() {
                WindowSurface::Wayland(toplevel) => {
                    self.surface_index.remove(toplevel.wl_surface());
                }
                #[cfg(feature = "xwayland")]
                WindowSurface::X11(surface) => {
                    self.x11_index.remove(&surface.window_id());
                }
            }
        }
        self.stacking.retain(|other| *other != id);
        self.model.remove(id);
        let fallback = self.stacking.last().copied();
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
                WindowRequest::Move => self.begin_move(state, id),
                WindowRequest::Resize(edges) => self.begin_resize(id, edges),
                WindowRequest::Activate => {
                    self.focus(state, Some(id));
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
            self.pre_overview_focus = id;
            if let Some(id) = id {
                if self.mode == SessionMode::Gnome {
                    self.stacking.retain(|other| *other != id);
                    self.stacking.push(id);
                }
            }
            return true;
        }
        self.apply_focus(state, id);
        true
    }

    fn apply_focus(&mut self, state: &mut State, id: Option<u64>) {
        let previous = self.model.focused();
        self.model.set_focused(id);
        if let Some(id) = id {
            // Floating stacks raise focus to the top; the strip keeps
            // column order independent of focus (niri shape), so focus
            // never reorders in scroll mode.
            if self.mode == SessionMode::Gnome {
                self.stacking.retain(|other| *other != id);
                self.stacking.push(id);
            }
        }
        let serial = SERIAL_COUNTER.next_serial();
        if let Some(previous) = previous {
            if Some(previous) != id {
                self.configure(previous, false);
            }
        }
        if let Some(id) = id {
            self.configure(id, true);
            if let (Some(keyboard), Some(window)) = (self.keyboard.clone(), self.windows.get(&id)) {
                // Unassociated X11 windows contribute no surface yet;
                // the model focus stands and the seat follows on
                // association via the reconcile re-sync.
                let focus = window
                    .surface
                    .wl_surface()
                    .map(|surface| surface.into_owned());
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
                Self::configure_wayland(toplevel, &layout, &window.geometry, activated);
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
        geometry: &Rectangle<i32, Logical>,
        activated: bool,
    ) {
        surface.with_pending_state(|pending| {
            pending.size = Some(geometry.size);
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
            ] {
                pending.states.unset(state);
            }
            match layout {
                WindowLayout::Floating | WindowLayout::Strip => {}
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
        if self.mode == SessionMode::Scroll {
            return;
        }
        let pointer = self.pointer_pos;
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
                pointer_start: self.pointer_pos,
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
            let pos = self.pointer_pos;
            let size = state.primary_size();
            if pos.y <= f64::from(WORK_AREA_TOP) + SNAP_EDGE_PX {
                self.set_maximized(state, id, true);
            } else if pos.x <= SNAP_EDGE_PX {
                self.set_tiled(state, id, TileSide::Left);
            } else if pos.x >= f64::from(size.w) - 1.0 - SNAP_EDGE_PX {
                self.set_tiled(state, id, TileSide::Right);
            }
        }
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
            crate::layer::topmost_layer_at(state, pos.x as i32, pos.y as i32)
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
            crate::layer::topmost_layer_at(state, pos.x as i32, pos.y as i32)
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
        if let Some(id) = self.window_at(pos) {
            if self.model.focused() != Some(id) {
                self.apply_focus(state, Some(id));
            }
        }
        if let (Some(pointer), Some(focused)) = (
            self.pointer.clone(),
            self.model.focused().and_then(|id| self.windows.get(&id)),
        ) {
            let surface = focused
                .surface
                .wl_surface()
                .map(|surface| surface.into_owned());
            // Focus point is the surface origin: smithay reports
            // surface-local coordinates as event minus focus.
            // The window rect is its xdg geometry; the surface origin
            // sits up-left of it by the client-side shadow.
            let origin = match surface.as_ref() {
                Some(s) => crate::popup::surface_origin(s, focused.geometry.loc),
                None => focused.geometry.loc,
            };
            pointer.motion(
                state,
                surface.map(|surface| (surface, (origin.x as f64, origin.y as f64).into())),
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
    }

    /// Pointer button: deliver to the focused window and focus the
    /// window under the cursor on press (click-to-focus). A press on a
    /// layer-shell surface moves keyboard focus there (unless the
    /// overview park owns it) so panel menus and banners take keys; the
    /// button itself follows pointer focus from the last motion.
    pub fn pointer_button(&mut self, state: &mut State, button: u32, pressed: bool, time: u32) {
        // Release ends a move/resize grab. It is still delivered (unless
        // its press was swallowed): the client's press opened smithay's
        // implicit click grab, and only the matching release closes it.
        // Dropping it pinned pointer focus to the dragged window for
        // good, so the panel never saw another click (#97).
        if !pressed && self.grab.is_some() {
            self.end_grab(state);
        }
        // Super+press on a window starts a move (GNOME's Super+drag).
        if pressed && self.super_held && !self.overview_open {
            let pos = self.pointer_pos;
            if crate::layer::topmost_layer_at(state, pos.x as i32, pos.y as i32).is_none()
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
                crate::layer::topmost_layer_at(state, pos.x as i32, pos.y as i32)
            {
                if !self.overview_open {
                    let serial = SERIAL_COUNTER.next_serial();
                    if let Some(keyboard) = self.keyboard.clone() {
                        keyboard.set_focus(state, Some(surface.clone()), serial);
                    }
                    state.sync_selection_focus(Some(&surface));
                }
            } else if self.overview_open {
                // Overview presses are the runtime's (preview hits).
            } else if let Some(id) = self.window_at(pos) {
                if self.model.focused() != Some(id) {
                    self.apply_focus(state, Some(id));
                }
            }
        }
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
        // Smithay's keyboard input takes XKB codespace (evdev + 8):
        // it feeds the code straight to xkbcommon and sends
        // `raw - 8` on the wire, so passing our evdev tables through
        // unchanged would mistranslate every key and panic on codes
        // below 8 (Escape, digits) once a client holds keyboard focus.
        keyboard.input::<(), _>(
            state,
            keycode.saturating_add(XKB_X11_OFFSET).into(),
            if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            SERIAL_COUNTER.next_serial(),
            time,
            |_, _, _| FilterResult::Forward,
        );
        true
    }

    /// Move a window by a delta, keeping it on its workspace. A manual
    /// move from a managed layout restores the stashed geometry first
    /// (GNOME drag-off shape), then applies the delta.
    pub fn move_window(&mut self, id: u64, dx: i32, dy: i32) -> bool {
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

    /// One strip column rectangle for `id`: full work-area height,
    /// preset-proportion width with gaps between columns (niri
    /// gaps-twice formula). Columns past the right edge overflow;
    /// the strip never squeezes to fit.
    fn strip_column_area(&self, state: &State, id: u64) -> Rectangle<i32, Logical> {
        let work = Self::work_area(state);
        let columns = self.strip_columns(state, id);
        let mut x = work.loc.x - self.strip_offset as i32;
        let mut width = Self::strip_width(work.size.w, self.strip_proportion(id));
        for (other, w) in &columns {
            if *other == id {
                width = *w;
                break;
            }
            x += w + Self::STRIP_GAP;
        }
        Rectangle {
            loc: (x, work.loc.y).into(),
            size: (width, work.size.h).into(),
        }
    }

    /// Current strip view offset (scroll mode), for tests.
    pub fn strip_offset(&self) -> f64 {
        self.strip_offset
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
        let total = Self::strip_total(&widths);
        let max = (total - work.size.w).max(0) as f64;
        let next = (self.strip_offset + horizontal + vertical).clamp(0.0, max);
        if next == self.strip_offset {
            return true;
        }
        self.strip_offset = next;
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
        let total = Self::strip_total(&widths);
        let max = (total - work.size.w).max(0) as f64;
        self.strip_offset = self.strip_offset.clamp(0.0, max);
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
            self.model
                .window(*id)
                .is_some_and(|entry| entry.workspace == workspace)
        });
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
    /// Pointer axis (wheel / scroll) motion, in backend units (v120
    /// for wheels, pixels for continuous devices). Only scroll mode
    /// consumes it; everywhere else it is dropped as before.
    Axis {
        horizontal: f64,
        vertical: f64,
        time: u32,
    },
}

/// X11 keycodes are kernel evdev numbers plus 8; the winit backend
/// normalizes back to evdev in [`translate_input`] so every table in
/// this file stays in evdev keycodes on all backends.
pub const XKB_X11_OFFSET: u32 = 8;

/// Overview trigger keycodes (evdev, 002 R1).
pub const SUPER_LEFT_KEYCODE: u32 = 125;
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
/// Strip preset-cycle key (evdev): Super+R steps the focused column
/// forward through the width presets, Super+Shift+R backward. Only
/// consumed in scroll mode; in gnome mode the press reaches clients
/// exactly as before (no R binding exists today).
pub const R_KEYCODE: u32 = 19;
/// Top inset of the maximized/tiled work area: the shell panel strip
/// (matches shell-host `PANEL_HEIGHT` and the Activities-strip
/// trigger height above).
pub const WORK_AREA_TOP: i32 = 32;
/// Hot-corner trigger region in logical pixels from the top-left.
pub const HOT_CORNER_PX: f64 = 8.0;
/// Activities-strip trigger height: the top strip the panel owns
/// (matches shell `PANEL_HEIGHT`).
pub const ACTIVITIES_STRIP_PX: f64 = 32.0;
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
}

impl TriggerState {
    /// Decide the overview action for one input event. `overview_open`
    /// is the hub intent; `pointer` is the last known pointer position
    /// for strip clicks (buttons carry no position).
    pub fn feed(
        &mut self,
        input: &ManagerInput,
        overview_open: bool,
        pointer: Point<f64, Logical>,
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
            ManagerInput::Motion { pos, .. } => {
                self.super_armed = false;
                if !overview_open && pos.x < HOT_CORNER_PX && pos.y < HOT_CORNER_PX {
                    TriggerAction::Open
                } else {
                    TriggerAction::None
                }
            }
            ManagerInput::Button { pressed, .. } => {
                let strip =
                    pressed && pointer.y < ACTIVITIES_STRIP_PX && pointer.x < ACTIVITIES_WIDTH_PX;
                self.super_armed = false;
                if strip {
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
        }
    }
}

impl WindowManager {
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
                if keycode == TAB_KEYCODE && self.alt_held {
                    // The whole chord stays invisible to apps: taps queue
                    // steps, releases are swallowed (an app that never saw
                    // the press must not see the release either).
                    if pressed {
                        self.switcher_open = true;
                        self.push_switcher(SwitcherAction::Step {
                            forward: !self.shift_held,
                        });
                    }
                } else if keycode == F4_KEYCODE && self.alt_held {
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
                    if !pressed && self.switcher_open && self.is_alt(keycode) {
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
        }
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
        if pressed && self.super_held && self.shift_held && keycode == T_KEYCODE {
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
        } else if pressed
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
            }
        } else if pressed && self.super_held && Self::is_arrow(keycode) {
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
        }
    }

    /// Queue one switcher drive event with a greppable trail for the
    /// CI journey (an empty switcher renders nothing, so pixels alone
    /// cannot prove the drive arrived).
    fn push_switcher(&mut self, action: SwitcherAction) {
        eprintln!("roost-compositor: switcher {action:?}");
        self.switcher_queue.push(action);
    }
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
