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

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, ButtonState, Event as BackendEvent, InputEvent, KeyState,
            KeyboardKeyEvent, PointerButtonEvent,
        },
        winit::WinitInput,
    },
    input::{
        keyboard::{FilterResult, KeyboardHandle, XkbConfig},
        pointer::{ButtonEvent, MotionEvent, PointerHandle},
    },
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::xdg::{ToplevelSurface, XdgToplevelSurfaceData},
    },
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;

use crate::{
    state::{StateModel, WindowUpdate},
    State, WindowRequest,
};
use rwd_shell_control::SwitcherAction;

/// Default floating size for a newly mapped window.
const DEFAULT_WIDTH: i32 = 800;
const DEFAULT_HEIGHT: i32 = 600;
/// Cascade offset for each newly mapped window.
const CASCADE_STEP: i32 = 32;

/// One managed window: live surface plus compositor-side geometry.
#[derive(Debug)]
struct ManagedWindow {
    surface: ToplevelSurface,
    geometry: Rectangle<i32, Logical>,
    /// Current presentation layout (floating, maximized, tiled, or
    /// fullscreen). Manager-local like geometry: the shell never sees
    /// it, it only sees the resulting geometry through rendering.
    layout: WindowLayout,
    /// Geometry to restore when leaving a non-floating layout. Stashed
    /// on leaving floating, cleared by manual move/resize (a fresh
    /// start, GNOME shape).
    restore: Option<Rectangle<i32, Logical>>,
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
}

/// Which work-area half a tiled window fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileSide {
    /// Left half.
    Left,
    /// Right half.
    Right,
}

/// Floating window manager over one workspace.
///
/// Owns the [`StateModel`] and the surface↔model index. The [`State`]
/// protocol object stays the caller: every method takes the seat handles
/// it needs from `state`, so input delivery, focus, and configure
/// round-trips share one call path for live events and tests.
pub struct WindowManager {
    model: StateModel,
    windows: HashMap<u64, ManagedWindow>,
    surface_index: HashMap<WlSurface, u64>,
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
            stacking: Vec::new(),
            cascade: 0,
            keyboard,
            pointer,
            pointer_pos: (0.0, 0.0).into(),
            super_held: false,
            shift_held: false,
            alt_held: false,
            switcher_open: false,
            switcher_queue: Vec::new(),
        }
    }

    /// Compositor-owned state model (revisioned windows/focus).
    pub fn model(&self) -> &StateModel {
        &self.model
    }

    /// Mutable model for control-command application.
    pub fn model_mut(&mut self) -> &mut StateModel {
        &mut self.model
    }

    /// Windows bottom-to-top with their geometry, for frame production.
    /// Only the active workspace renders; other workspaces keep their
    /// surfaces mapped but hidden.
    pub fn visible_windows(&self) -> Vec<(ToplevelSurface, Rectangle<i32, Logical>)> {
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

    /// Reconcile live surfaces with the model: map new toplevels, drop
    /// dead ones, sync titles, and drain client window-state requests.
    /// Called once per loop tick after client dispatch, and directly
    /// by tests.
    pub fn reconcile(&mut self, state: &mut State) {
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
        let gone: Vec<u64> = self
            .surface_index
            .iter()
            .filter(|(wl, _)| !wl.is_alive())
            .map(|(_, id)| *id)
            .collect();
        for id in gone {
            self.unmap(state, id);
        }
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
        let _ = seen;
    }

    /// Register one toplevel: model insert with cascaded geometry,
    /// initial configure, and focus. Returns the model id.
    fn map(&mut self, state: &mut State, surface: &ToplevelSurface) -> u64 {
        let title = read_title(surface).unwrap_or_else(|| "untitled".to_owned());
        let app_id = read_app_id(surface);
        let id = self
            .model
            .insert(&title, app_id.as_deref(), self.model.active_workspace());
        let offset = self.cascade % 320;
        self.cascade = self.cascade.wrapping_add(CASCADE_STEP);
        let geometry = Rectangle {
            loc: (offset, offset).into(),
            size: (DEFAULT_WIDTH, DEFAULT_HEIGHT).into(),
        };
        self.windows.insert(
            id,
            ManagedWindow {
                surface: surface.clone(),
                geometry,
                layout: WindowLayout::Floating,
                restore: None,
            },
        );
        self.surface_index.insert(surface.wl_surface().clone(), id);
        self.stacking.push(id);
        self.place_transient(surface, id);
        self.configure(id, true);
        self.apply_focus(state, Some(id));
        id
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
    /// to the topmost remaining window, if any.
    fn unmap(&mut self, state: &mut State, id: u64) {
        if let Some(window) = self.windows.remove(&id) {
            self.surface_index.remove(window.surface.wl_surface());
        }
        self.stacking.retain(|other| *other != id);
        self.model.remove(id);
        let fallback = self.stacking.last().copied();
        self.apply_focus(state, fallback);
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
        self.apply_focus(state, id);
        true
    }

    fn apply_focus(&mut self, state: &mut State, id: Option<u64>) {
        let previous = self.model.focused();
        self.model.set_focused(id);
        if let Some(id) = id {
            self.stacking.retain(|other| *other != id);
            self.stacking.push(id);
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
                keyboard.set_focus(state, Some(window.surface.wl_surface().clone()), serial);
            }
        } else if let Some(keyboard) = self.keyboard.clone() {
            keyboard.set_focus(state, None, serial);
        }
        // Clipboard offers follow keyboard focus, with or without a
        // keyboard capability attached.
        let surface = id.and_then(|id| {
            self.windows
                .get(&id)
                .map(|window| window.surface.wl_surface().clone())
        });
        state.sync_device_focus(surface.as_ref());
    }

    /// Send a configure advertising this window's geometry, activation
    /// flag, and presentation state.
    fn configure(&self, id: u64, activated: bool) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        let layout = window.layout;
        window.surface.with_pending_state(|pending| {
            pending.size = Some(window.geometry.size);
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
                WindowLayout::Floating => {}
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
        window.surface.send_configure();
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
    pub fn pointer_motion(&mut self, state: &mut State, pos: Point<f64, Logical>, time: u32) {
        self.pointer_pos = pos;
        if let Some(id) = self.window_at(pos) {
            if self.model.focused() != Some(id) {
                self.apply_focus(state, Some(id));
            }
        }
        if let (Some(pointer), Some(focused)) = (
            self.pointer.clone(),
            self.model.focused().and_then(|id| self.windows.get(&id)),
        ) {
            let surface = focused.surface.wl_surface().clone();
            let origin = focused.geometry.loc;
            let local = (pos.x - origin.x as f64, pos.y - origin.y as f64);
            pointer.motion(
                state,
                Some((surface, local.into())),
                &MotionEvent {
                    location: pos,
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
        }
    }

    /// Pointer button: deliver to the focused window and focus the
    /// window under the cursor on press (click-to-focus).
    pub fn pointer_button(&mut self, state: &mut State, button: u32, pressed: bool, time: u32) {
        if pressed {
            if let Some(id) = self.window_at(self.pointer_pos) {
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
        window.surface.send_close();
        eprintln!("rwd-compositor: window {id} close requested");
        true
    }

    /// Close the focused window, if any.
    pub fn close_focused(&self) -> bool {
        self.model.focused().is_some_and(|id| self.close_window(id))
    }

    /// Work area a maximized window fills: the output minus the shell
    /// panel strip ([`WORK_AREA_TOP`]).
    fn work_area(state: &State) -> Rectangle<i32, Logical> {
        let (w, h) = (state.output_size.w.max(0), state.output_size.h.max(0));
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
        Rectangle {
            loc: (0, 0).into(),
            size: (state.output_size.w.max(0), state.output_size.h.max(0)).into(),
        }
    }

    /// Maximize a window into the work area, stashing its floating
    /// geometry for restore. Idempotent; false for unknown ids.
    pub fn set_maximized(&mut self, state: &mut State, id: u64, maximized: bool) -> bool {
        if maximized {
            self.apply_layout(state, id, WindowLayout::Maximized)
        } else {
            self.restore_window(id)
        }
    }

    /// Cover the output with a window, stashing its floating geometry.
    /// Idempotent; false for unknown ids.
    pub fn set_fullscreen(&mut self, state: &mut State, id: u64, fullscreen: bool) -> bool {
        if fullscreen {
            self.apply_layout(state, id, WindowLayout::Fullscreen)
        } else {
            self.restore_window(id)
        }
    }

    /// Tile a window into one work-area half, stashing its floating
    /// geometry. Idempotent; false for unknown ids.
    pub fn set_tiled(&mut self, state: &mut State, id: u64, side: TileSide) -> bool {
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
            eprintln!("rwd-compositor: window {id} Floating");
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
        eprintln!("rwd-compositor: window {id} {layout:?}");
        true
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
/// Top inset of the maximized/tiled work area: the shell panel strip
/// (matches shell-host `PANEL_HEIGHT` and the Activities-strip
/// trigger height above).
pub const WORK_AREA_TOP: i32 = 32;
/// Hot-corner trigger region in logical pixels from the top-left.
pub const HOT_CORNER_PX: f64 = 8.0;
/// Activities-strip trigger: button presses in the top strip open the
/// overview (the panel owns that strip; matches shell `PANEL_HEIGHT`).
pub const ACTIVITIES_STRIP_PX: f64 = 32.0;

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
    /// is the hub intent; `pointer_y` is the last known pointer height
    /// for strip clicks (buttons carry no position).
    pub fn feed(
        &mut self,
        input: &ManagerInput,
        overview_open: bool,
        pointer_y: f64,
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
                let strip = pressed && pointer_y < ACTIVITIES_STRIP_PX;
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
        if pressed
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
                    "rwd-compositor: workspace now {}",
                    self.model.active_workspace()
                );
            }
        } else if pressed && self.super_held && Self::is_arrow(keycode) {
            if let Some(id) = self.model.focused() {
                match keycode {
                    ARROW_UP_KEYCODE => {
                        self.set_maximized(state, id, true);
                    }
                    ARROW_DOWN_KEYCODE => {
                        self.restore_window(id);
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
        eprintln!("rwd-compositor: switcher {action:?}");
        self.switcher_queue.push(action);
    }
}

/// Translate a winit backend [`InputEvent`] into [`ManagerInput`].
/// Returns `None` for device add/remove and other unconsumed kinds.
pub fn translate_input(event: InputEvent<WinitInput>) -> Option<ManagerInput> {
    match event {
        InputEvent::Keyboard { event } => Some(ManagerInput::Key {
            // The winit backend reports X11 keycodes (kernel evdev
            // number plus 8); normalize back to evdev so trigger and
            // overlay tables written in evdev keycodes match on every
            // backend. X keycodes below 8 cannot occur; saturate.
            keycode: u32::from(event.key_code()).saturating_sub(XKB_X11_OFFSET),
            pressed: event.state() == KeyState::Pressed,
            time: (event.time() / 1000) as u32,
        }),
        InputEvent::PointerMotionAbsolute { event } => {
            let pos = event.position();
            Some(ManagerInput::Motion {
                pos: (pos.x, pos.y).into(),
                time: (event.time() / 1000) as u32,
            })
        }
        InputEvent::PointerButton { event } => Some(ManagerInput::Button {
            button: event.button_code(),
            pressed: event.state() == ButtonState::Pressed,
            time: (event.time() / 1000) as u32,
        }),
        _ => None,
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
    fn lone_super_tap_toggles() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, true), false, 100.0),
            TriggerAction::None,
            "press alone arms without acting"
        );
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, false), false, 100.0),
            TriggerAction::Toggle
        );
        // Right Super taps too.
        assert_eq!(
            triggers.feed(&key(SUPER_RIGHT_KEYCODE, true), true, 100.0),
            TriggerAction::None
        );
        assert_eq!(
            triggers.feed(&key(SUPER_RIGHT_KEYCODE, false), true, 100.0),
            TriggerAction::Toggle
        );
    }

    #[test]
    fn super_combo_does_not_toggle() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, true), false, 100.0),
            TriggerAction::None
        );
        // Any other key in between cancels the tap (Super+T etc.).
        assert_eq!(
            triggers.feed(&key(20, true), false, 100.0),
            TriggerAction::None
        );
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, false), false, 100.0),
            TriggerAction::None,
            "release after a combo is not a tap"
        );
    }

    #[test]
    fn hot_corner_opens_only_when_closed() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&motion(2.0, 3.0), false, 3.0),
            TriggerAction::Open
        );
        assert_eq!(
            triggers.feed(&motion(2.0, 3.0), true, 3.0),
            TriggerAction::None,
            "no re-open while already open"
        );
        assert_eq!(
            triggers.feed(&motion(400.0, 300.0), false, 300.0),
            TriggerAction::None
        );
    }

    #[test]
    fn strip_click_toggles_and_disarms_super() {
        let mut triggers = TriggerState::default();
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, true), false, 100.0),
            TriggerAction::None
        );
        // Click in the Activities strip: toggles, and the earlier Super
        // press must not linger as an armed tap.
        assert_eq!(
            triggers.feed(&button(true), false, 10.0),
            TriggerAction::Toggle
        );
        assert_eq!(
            triggers.feed(&key(SUPER_LEFT_KEYCODE, false), false, 100.0),
            TriggerAction::None,
            "strip click consumed the armed Super"
        );
        // Clicks below the strip do nothing.
        assert_eq!(
            triggers.feed(&button(true), false, 200.0),
            TriggerAction::None
        );
        // Releases never toggle.
        assert_eq!(
            triggers.feed(&button(false), false, 10.0),
            TriggerAction::None
        );
    }
}
