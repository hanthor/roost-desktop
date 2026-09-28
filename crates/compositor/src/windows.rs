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

/// Overview trigger keycodes (evdev, 002 R1).
pub const SUPER_LEFT_KEYCODE: u32 = 125;
pub const SUPER_RIGHT_KEYCODE: u32 = 126;
pub const ESCAPE_KEYCODE: u32 = 1;
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
