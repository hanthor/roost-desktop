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
    State,
};

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
}

impl WindowManager {
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
    pub fn visible_windows(&self) -> Vec<(ToplevelSurface, Rectangle<i32, Logical>)> {
        self.stacking
            .iter()
            .filter_map(|id| {
                self.windows
                    .get(id)
                    .map(|w| (w.surface.clone(), w.geometry))
            })
            .collect()
    }

    /// Reconcile live surfaces with the model: map new toplevels, drop
    /// dead ones, and sync titles. Called once per loop tick after client
    /// dispatch, and directly by tests.
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
        let _ = seen;
    }

    /// Register one toplevel: model insert with cascaded geometry,
    /// initial configure, and focus. Returns the model id.
    fn map(&mut self, state: &mut State, surface: &ToplevelSurface) -> u64 {
        let title = read_title(surface).unwrap_or_else(|| "untitled".to_owned());
        let app_id = read_app_id(surface);
        let id = self.model.insert(&title, app_id.as_deref(), 0);
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
            },
        );
        self.surface_index.insert(surface.wl_surface().clone(), id);
        self.stacking.push(id);
        self.configure(id, true);
        self.apply_focus(state, Some(id));
        id
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
    }

    /// Send a configure advertising this window's geometry and
    /// activation flag.
    fn configure(&self, id: u64, activated: bool) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        window.surface.with_pending_state(|pending| {
            pending.size = Some(window.geometry.size);
            if activated {
                pending.states.set(xdg_toplevel::State::Activated);
            } else {
                pending.states.unset(xdg_toplevel::State::Activated);
            }
        });
        window.surface.send_configure();
    }

    /// Topmost window containing `pos`, if any.
    pub fn window_at(&self, pos: Point<f64, Logical>) -> Option<u64> {
        self.stacking.iter().rev().copied().find(|id| {
            self.windows
                .get(id)
                .is_some_and(|w| contains(w.geometry, pos))
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
        keyboard.input::<(), _>(
            state,
            keycode.into(),
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

    /// Move a window by a delta, keeping it on its workspace.
    pub fn move_window(&mut self, id: u64, dx: i32, dy: i32) -> bool {
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        let loc = window.geometry.loc;
        window.geometry.loc = (loc.x + dx, loc.y + dy).into();
        true
    }

    /// Resize a window and advertise the new size via configure.
    pub fn resize_window(&mut self, id: u64, width: i32, height: i32) -> bool {
        if width <= 0 || height <= 0 {
            return false;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            return false;
        };
        window.geometry.size = (width, height).into();
        self.configure(id, self.model.focused() == Some(id));
        true
    }

    /// Move a window to another workspace (the single-workspace slice
    /// still tracks membership for the shell contract).
    pub fn move_to_workspace(&mut self, id: u64, workspace: u32) -> bool {
        if !self.windows.contains_key(&id) {
            return false;
        }
        self.model.update(
            id,
            WindowUpdate {
                workspace: Some(workspace),
                ..Default::default()
            },
        )
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

fn contains(geometry: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> bool {
    let x = geometry.loc.x as f64;
    let y = geometry.loc.y as f64;
    pos.x >= x
        && pos.y >= y
        && pos.x < x + geometry.size.w as f64
        && pos.y < y + geometry.size.h as f64
}

/// Backend input event kinds this manager consumes from the runtime.
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

impl WindowManager {
    /// Dispatch one backend input event into focus and delivery.
    pub fn on_input(&mut self, state: &mut State, input: ManagerInput) {
        match input {
            ManagerInput::Key {
                keycode,
                pressed,
                time,
            } => {
                self.keyboard_key(state, keycode, pressed, time);
            }
            ManagerInput::Motion { pos, time } => self.pointer_motion(state, pos, time),
            ManagerInput::Button {
                button,
                pressed,
                time,
            } => self.pointer_button(state, button, pressed, time),
        }
    }
}

/// Translate a winit backend [`InputEvent`] into [`ManagerInput`].
/// Returns `None` for device add/remove and other unconsumed kinds.
pub fn translate_input(event: InputEvent<WinitInput>) -> Option<ManagerInput> {
    match event {
        InputEvent::Keyboard { event } => Some(ManagerInput::Key {
            keycode: event.key_code().into(),
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
