//! Protocols beyond the core slice (#89): what GTK4, libadwaita and
//! Chromium look for on a GNOME 51 session.
//!
//! - linux-dmabuf (v3, formats from the renderer): GPU clients and the
//!   GTK4 GL renderer stop falling back to software. The runtime
//!   creates the global once its renderer exists
//!   ([`State::enable_dmabuf`]); buffers import lazily at draw time.
//! - xdg-activation v1: an app may hand focus to another only with a
//!   token minted while the requesting client held keyboard focus,
//!   used within [`ACTIVATION_TOKEN_TTL`] (Mutter's focus-stealing
//!   shape). Valid requests focus and raise the window.
//! - viewporter, fractional-scale v1 (preferred scale follows the target output),
//!   single-pixel-buffer, cursor-shape v1.
//! - idle-inhibit: a live inhibitor holds off the idle lock (video
//!   players, presentations).
//!
//! - text-input v3 and input-method v2 (#60): IMEs such as fcitx5 type
//!   into GTK apps; smithay relays focus and text, candidate popups are
//!   tracked like any other popup. The compositor's IBus bridge also
//!   hears where the text cursor is on screen: after smithay reports
//!   the cursor rectangle to its input popup (in the text field's
//!   surface coordinates, all input-method-v2 offers), the compositor
//!   reports it again in global coordinates, which IBus's
//!   `SetCursorLocation` wants (GNOME Shell knows them first-hand).
//! - pointer-gestures, relative-pointer and pointer-constraints (#60,
//!   #89): touchpad gestures reach apps, games get raw motion and a
//!   locked pointer.
//!
//! - xdg-dialog v1: a modal dialog is attached to its parent as in
//!   GNOME (attach-modal-dialogs): focusing the parent focuses the
//!   dialog. xdg-foreign v2 lets the portal parent its dialogs to the
//!   app's window the same way.
//! - xdg-system-bell v1: rings GNOME's `bell-window-system` theme sound
//!   (Mutter's audible bell) through libcanberra's player.
//! - xdg-toplevel-tag v1: each window keeps its tag and description.
//! - keyboard-shortcuts-inhibit v1: virtual machines and remote desktops
//!   get every key while focused; Super+Escape (Mutter's
//!   restore-shortcuts) hands the shortcuts back until the window is
//!   focused again. Granted at once: Roost has no GNOME Shell
//!   permission dialog to ask with.
//! - pointer-warp v1: a client may move the pointer within its own
//!   surface while it has pointer focus (the enter serial must match).
//! - presentation-time, fifo and commit-timing: see [`crate::frame_timing`].
//!
//! Deliberately absent, matching Mutter: xdg-decoration (GNOME is
//! client-side decorations only).

use std::time::{Duration, Instant};

use smithay::{
    backend::allocator::{dmabuf::Dmabuf, Format},
    delegate_cursor_shape, delegate_dmabuf, delegate_fractional_scale, delegate_idle_inhibit,
    delegate_input_method_manager, delegate_pointer_constraints, delegate_pointer_gestures,
    delegate_relative_pointer, delegate_single_pixel_buffer, delegate_text_input_manager,
    delegate_viewporter, delegate_xdg_activation, delegate_xdg_dialog, delegate_xdg_foreign,
    delegate_xdg_system_bell, delegate_xdg_toplevel_tag,
    input::pointer::PointerHandle,
    reexports::{
        wayland_protocols::{
            wp::pointer_warp::v1::server::wp_pointer_warp_v1::{self, WpPointerWarpV1},
            xdg::shell::server::xdg_toplevel::XdgToplevel,
        },
        wayland_server::{
            backend::GlobalId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch,
            DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Logical, Point, Rectangle},
    wayland::{
        compositor::with_states,
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fractional_scale::{
            with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
        },
        idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState},
        input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface as ImPopup},
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor, KeyboardShortcutsInhibitorSeat,
        },
        pointer_constraints::{
            with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
        },
        pointer_gestures::PointerGesturesState,
        relative_pointer::RelativePointerManagerState,
        shell::xdg::dialog::{XdgDialogHandler, XdgDialogState},
        single_pixel_buffer::SinglePixelBufferState,
        tablet_manager::TabletSeatHandler,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
        xdg_foreign::{XdgForeignHandler, XdgForeignState},
        xdg_system_bell::{XdgSystemBellHandler, XdgSystemBellState},
        xdg_toplevel_tag::{XdgToplevelTagHandler, XdgToplevelTagManager},
    },
};

use crate::{State, WindowRequest};

/// How long an activation token stays usable (Mutter allows a few
/// seconds between the launching click and the new window's request).
pub const ACTIVATION_TOKEN_TTL: Duration = Duration::from_secs(10);

/// Protocol states held for the lifetime of the display.
pub(crate) struct Protocols {
    pub(crate) dmabuf_state: DmabufState,
    pub(crate) dmabuf_global: Option<DmabufGlobal>,
    pub(crate) activation_state: XdgActivationState,
    _viewporter: ViewporterState,
    _fractional_scale: FractionalScaleManagerState,
    _single_pixel_buffer: SinglePixelBufferState,
    _cursor_shape: CursorShapeManagerState,
    _idle_inhibit: IdleInhibitManagerState,
    _text_input: TextInputManagerState,
    _input_method: InputMethodManagerState,
    _gestures: PointerGesturesState,
    _relative_pointer: RelativePointerManagerState,
    _pointer_constraints: PointerConstraintsState,
    _virtual_keyboard: smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState,
    _dialog: XdgDialogState,
    _system_bell: XdgSystemBellState,
    _toplevel_tag: XdgToplevelTagManager,
    pub(crate) foreign: XdgForeignState,
    pub(crate) shortcuts_inhibit: KeyboardShortcutsInhibitState,
    _pointer_warp: GlobalId,
    /// Pointer warps clients asked for, drained by the window manager:
    /// surface, surface-local position, enter serial.
    pub(crate) pointer_warps: Vec<(WlSurface, Point<f64, Logical>, u32)>,
    /// The system bell (xdg-system-bell).
    pub(crate) bell: Bell,
    /// Surfaces holding an idle inhibitor.
    pub(crate) inhibitors: Vec<WlSurface>,
    /// Scale fractional-scale clients are asked to render at (#59).
    pub(crate) preferred_scale: f64,
}

impl Protocols {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            activation_state: XdgActivationState::new::<State>(dh),
            _viewporter: ViewporterState::new::<State>(dh),
            _fractional_scale: FractionalScaleManagerState::new::<State>(dh),
            _single_pixel_buffer: SinglePixelBufferState::new::<State>(dh),
            _cursor_shape: CursorShapeManagerState::new::<State>(dh),
            _idle_inhibit: IdleInhibitManagerState::new::<State>(dh),
            _text_input: TextInputManagerState::new::<State>(dh),
            // Any client may become the input method, as on GNOME.
            _input_method: InputMethodManagerState::new::<State, _>(dh, |_client| true),
            _gestures: PointerGesturesState::new::<State>(dh),
            _relative_pointer: RelativePointerManagerState::new::<State>(dh),
            _pointer_constraints: PointerConstraintsState::new::<State>(dh),
            // Only the compositor's IBus bridge may type for others: it
            // returns the keys IBus does not take (GNOME offers no such
            // protocol at all).
            _virtual_keyboard: smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState::new::<
                State,
                _,
            >(dh, |client| {
                client
                    .get_data::<crate::ClientState>()
                    .is_some_and(|data| data.ime_bridge)
            }),
            _dialog: XdgDialogState::new::<State>(dh),
            _system_bell: XdgSystemBellState::new::<State>(dh),
            _toplevel_tag: XdgToplevelTagManager::new::<State>(dh),
            foreign: XdgForeignState::new::<State>(dh),
            shortcuts_inhibit: KeyboardShortcutsInhibitState::new::<State>(dh),
            _pointer_warp: dh.create_global::<State, WpPointerWarpV1, ()>(1, ()),
            pointer_warps: Vec::new(),
            bell: Bell::default(),
            inhibitors: Vec::new(),
            preferred_scale: 1.0,
        }
    }
}

/// The system bell: rings counted, sounds played through libcanberra's
/// `canberra-gtk-play` when it is installed, at most one at a time.
#[derive(Default)]
pub(crate) struct Bell {
    rings: u64,
    last: Option<Instant>,
    player: Option<std::process::Child>,
}

/// GNOME's bell sound (Mutter's `meta_bell_notify`).
pub const BELL_SOUND: &str = "bell-window-system";

impl Bell {
    fn ring(&mut self) {
        self.rings += 1;
        // A burst of rings is one sound, as a held key would otherwise
        // queue a sound per repeat.
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(100))
        {
            return;
        }
        self.last = Some(now);
        if let Some(child) = self.player.as_mut() {
            match child.try_wait() {
                Ok(None) => return,
                _ => self.player = None,
            }
        }
        if std::env::var_os("ROOST_BELL").is_some_and(|v| v == "0") {
            return;
        }
        self.player = std::process::Command::new("canberra-gtk-play")
            .args(["--id", BELL_SOUND, "--description", "Bell event"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
    }
}

/// A window's xdg-toplevel-tag: what it is to the app ("main window",
/// "settings") and a human description. Kept in the surface data.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ToplevelTag {
    /// Untranslated tag, stable across runs.
    pub tag: Option<String>,
    /// Translated description for people.
    pub description: Option<String>,
}

/// The tag and description a window's client gave it.
pub fn toplevel_tag(surface: &WlSurface) -> ToplevelTag {
    with_states(surface, |states| {
        states
            .data_map
            .get::<std::sync::Mutex<ToplevelTag>>()
            .map(|tag| tag.lock().unwrap().clone())
            .unwrap_or_default()
    })
}

fn update_tag(state: &State, toplevel: &XdgToplevel, update: impl FnOnce(&mut ToplevelTag)) {
    let Some(surface) = state.xdg_shell_state.get_toplevel(toplevel) else {
        return;
    };
    with_states(surface.wl_surface(), |states| {
        let tag = states
            .data_map
            .get_or_insert_threadsafe(|| std::sync::Mutex::new(ToplevelTag::default()));
        update(&mut tag.lock().unwrap());
    });
}

impl State {
    /// Advertise linux-dmabuf with the renderer's importable formats.
    /// No formats (software rendering) means no global: clients then
    /// use shm, exactly as before.
    pub fn enable_dmabuf(&mut self, formats: Vec<Format>) {
        if formats.is_empty() || self.protocols.dmabuf_global.is_some() {
            if formats.is_empty() {
                eprintln!("roost-compositor: no dmabuf formats, linux-dmabuf not advertised");
            }
            return;
        }
        eprintln!(
            "roost-compositor: linux-dmabuf v3 with {} formats",
            formats.len()
        );
        let dh = self.dh.clone();
        let global = self
            .protocols
            .dmabuf_state
            .create_global::<State>(&dh, formats);
        self.protocols.dmabuf_global = Some(global);
    }

    /// Scale for fractional-scale clients (the output scale, #59).
    pub fn set_preferred_scale(&mut self, scale: f64) {
        self.protocols.preferred_scale = scale;
    }

    /// Whether a live, mapped surface holds an idle inhibitor.
    pub fn idle_inhibited(&mut self) -> bool {
        self.protocols.inhibitors.retain(|s| s.is_alive());
        !self.protocols.inhibitors.is_empty()
    }

    /// Where `surface`'s buffer origin lands globally: a mapped
    /// toplevel's (its window rect less the client's shadow margin) or
    /// a layer surface's.
    fn surface_origin(&self, surface: &WlSurface) -> Option<Point<i32, Logical>> {
        if let Some(window) = self.window_origins.get(surface) {
            return Some(*window - crate::popup::window_geometry_loc(surface));
        }
        crate::layer::layer_layout(self)
            .into_iter()
            .find(|(s, _, _)| s == surface)
            .map(|(_, (x, y), _)| (x, y).into())
    }

    /// Times the system bell rang.
    pub fn bell_rings(&self) -> u64 {
        self.protocols.bell.rings
    }

    /// Whether the surface holding keyboard focus inhibits the
    /// compositor's shortcuts (keyboard-shortcuts-inhibit).
    pub fn shortcuts_inhibited(&self) -> bool {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return false;
        };
        self.seat
            .keyboard_shortcuts_inhibitor_for_surface(&focus)
            .is_some_and(|inhibitor| inhibitor.is_active())
    }

    /// Mutter's restore-shortcuts (Super+Escape): the focused surface's
    /// inhibitor goes inactive until it is focused again. Returns
    /// whether one was active.
    pub fn restore_shortcuts(&mut self) -> bool {
        let Some(focus) = self.seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return false;
        };
        match self.seat.keyboard_shortcuts_inhibitor_for_surface(&focus) {
            Some(inhibitor) if inhibitor.is_active() => {
                inhibitor.inactivate();
                true
            }
            _ => false,
        }
    }

    /// Keyboard focus moved to `surface`: an inhibitor the user
    /// suspended with restore-shortcuts takes effect again.
    pub(crate) fn reactivate_inhibitor(&self, surface: &WlSurface) {
        if let Some(inhibitor) = self.seat.keyboard_shortcuts_inhibitor_for_surface(surface) {
            if !inhibitor.is_active() {
                inhibitor.activate();
            }
        }
    }

    /// Drain the pointer warps clients asked for.
    pub(crate) fn take_pointer_warps(&mut self) -> Vec<(WlSurface, Point<f64, Logical>, u32)> {
        std::mem::take(&mut self.protocols.pointer_warps)
    }

    /// Client that currently holds keyboard focus, if any.
    fn keyboard_client(&self) -> Option<smithay::reexports::wayland_server::backend::ClientId> {
        let keyboard = self.seat.get_keyboard()?;
        let focus = keyboard.current_focus()?;
        focus.client().map(|c| c.id())
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.protocols.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        // The renderer lives with the backend: buffers import when a
        // frame first draws them, and a failed import only skips that
        // surface. The global only lists formats the renderer imports.
        let _ = notifier.successful::<State>();
    }
}
delegate_dmabuf!(State);

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.protocols.activation_state
    }

    /// Mint only for the client holding keyboard focus, and only with
    /// an input serial: a background app cannot mint its way to focus.
    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        data.serial.is_some()
            && data.client_id.is_some()
            && data.client_id == self.keyboard_client()
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // One use per token (ADR 0002's policy, now on the protocol).
        self.protocols.activation_state.remove_token(&token);
        if token_data.timestamp.elapsed() < ACTIVATION_TOKEN_TTL {
            self.window_requests
                .push((surface, WindowRequest::Activate));
        }
    }
}
delegate_xdg_activation!(State);

delegate_viewporter!(State);

impl State {
    pub(crate) fn initial_surface_scale(&self, surface: &WlSurface) -> f64 {
        let mut surface = surface.clone();
        // Bound traversal also protects against client-supplied parent cycles.
        for _ in 0..64 {
            if let Some(layer) = self.panel_surfaces.iter().find(|l| l.surface == surface) {
                let loc = self.loc_for_output(layer.output_name.as_deref());
                return self
                    .scale_for(smithay::utils::Rectangle::new(loc.into(), (1, 1).into()))
                    .fractional_scale();
            }
            if let Some(loc) = self.window_origins.get(&surface) {
                return self
                    .scale_for(smithay::utils::Rectangle::new(*loc, (1, 1).into()))
                    .fractional_scale();
            }
            let parent = self
                .xdg_shell_state
                .popup_surfaces()
                .iter()
                .find(|p| *p.wl_surface() == surface)
                .and_then(|p| p.get_parent_surface())
                .or_else(|| {
                    self.xdg_shell_state
                        .toplevel_surfaces()
                        .iter()
                        .find(|t| *t.wl_surface() == surface)
                        .and_then(|t| t.parent())
                });
            if let Some(parent) = parent {
                surface = parent;
                continue;
            }
            return self
                .initial_outputs
                .get(&surface)
                .and_then(|name| self.outputs.iter().find(|entry| &entry.name == name))
                .and_then(|entry| entry.output.as_ref())
                .map(|output| output.current_scale().fractional_scale())
                .unwrap_or_else(|| self.initial_window_scale());
        }
        self.initial_window_scale()
    }

    pub(crate) fn refresh_initial_surface_scale(&self, surface: &WlSurface) {
        let scale = self.initial_surface_scale(surface);
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Roles may already exist when the client creates its scale object.
        // Popups inherit their parent; layer surfaces use their bound output.
        if let Some(name) = self.new_window_output().map(|entry| entry.name.clone()) {
            self.initial_outputs.insert(surface.clone(), name);
        }
        let scale = self.initial_surface_scale(&surface);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}
delegate_fractional_scale!(State);

delegate_single_pixel_buffer!(State);

impl TabletSeatHandler for State {}
delegate_cursor_shape!(State);

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        if !self.protocols.inhibitors.contains(&surface) {
            self.protocols.inhibitors.push(surface);
        }
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.protocols.inhibitors.retain(|s| *s != surface);
    }
}
delegate_idle_inhibit!(State);

delegate_text_input_manager!(State);

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: ImPopup) {
        let _ = self
            .popups
            .track_popup(smithay::desktop::PopupKind::from(surface));
    }

    fn dismiss_popup(&mut self, surface: ImPopup) {
        if let Some(parent) = surface.get_parent().map(|p| p.surface.clone()) {
            let _ = smithay::desktop::PopupManager::dismiss_popup(
                &parent,
                &smithay::desktop::PopupKind::from(surface),
            );
        }
    }

    /// The text cursor moved. The IBus bridge's popup (and only its:
    /// other input methods keep the protocol's surface coordinates)
    /// hears the rectangle again, in global coordinates.
    fn popup_repositioned(&mut self, mut surface: ImPopup) {
        let is_bridge = surface.wl_surface().client().is_some_and(|c| {
            c.get_data::<crate::ClientState>()
                .is_some_and(|d| d.ime_bridge)
        });
        if !is_bridge {
            return;
        }
        let Some(parent) = surface.get_parent().map(|p| p.surface.clone()) else {
            return;
        };
        let Some(origin) = self.surface_origin(&parent) else {
            return;
        };
        // smithay just stored the field's rectangle (surface-local).
        let local = surface.text_input_rectangle();
        let global = global_cursor_rect(origin, local);
        surface.set_text_input_rectangle(global.loc.x, global.loc.y, global.size.w, global.size.h);
        // Placement stays relative to the parent, as smithay set it.
        surface.set_location(local.loc);
    }

    /// Where the text field's window sits, so the candidate popup
    /// lands next to the caret.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.window_origins
            .get(parent)
            .map(|loc| Rectangle::new(*loc, (0, 0).into()))
            .unwrap_or_default()
    }
}
delegate_input_method_manager!(State);

impl State {
    /// A moved parent changes the caret's global position even when the app
    /// does not send a new text-input rectangle.
    pub(crate) fn refresh_ime_cursor_origins(&mut self) {
        for parent in self.window_origins.keys() {
            for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(parent) {
                let smithay::desktop::PopupKind::InputMethod(mut surface) = popup else {
                    continue;
                };
                if !surface.wl_surface().client().is_some_and(|client| {
                    client
                        .get_data::<crate::ClientState>()
                        .is_some_and(|data| data.ime_bridge)
                }) {
                    continue;
                }
                let Some(origin) = self.surface_origin(parent) else {
                    continue;
                };
                let current = surface.text_input_rectangle();
                let local = surface.location();
                let global = global_cursor_rect(origin, Rectangle::new(local, current.size));
                if current != global {
                    surface.set_text_input_rectangle(
                        global.loc.x,
                        global.loc.y,
                        global.size.w,
                        global.size.h,
                    );
                    surface.set_location(local);
                }
            }
        }
    }
}
// Keep Smithay's manager/object state, but route the trusted bridge's
// returned keys through the physical keyboard map. Smithay 0.7's virtual
// keyboard sends a different KeymapFile before each returned key; GTK then
// resets its keyboard state while an Escape is closing a layer popup.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1:
    smithay::wayland::virtual_keyboard::VirtualKeyboardManagerGlobalData
] => smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1: ()
] => smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState);

impl Dispatch<
    smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<State>,
> for State {
    fn request(
        state: &mut State,
        client: &Client,
        resource: &smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        request: smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Request,
        data: &smithay::wayland::virtual_keyboard::VirtualKeyboardUserData<State>,
        handle: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    ) {
        use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::Request;
        use smithay::wayland::input_method::InputMethodKeyboardGrab;
        match request {
            Request::Key { time, key, state: pressed } => {
                let Some(keyboard) = state.seat.get_keyboard() else { return };
                // The physical event has already updated XKB. Do not update
                // it twice or send it back into the IM grab recursively.
                let grab = keyboard.with_grab(|serial, grab| {
                    grab.downcast_ref::<InputMethodKeyboardGrab>().cloned().map(|grab| (serial, grab))
                }).flatten();
                if grab.is_some() { keyboard.unset_grab(state); }
                keyboard.input_forward(state, key.saturating_add(8).into(),
                    if pressed == 1 { smithay::backend::input::KeyState::Pressed } else { smithay::backend::input::KeyState::Released },
                    smithay::utils::SERIAL_COUNTER.next_serial(), time, false);
                if let Some((serial, grab)) = grab { keyboard.set_grab(state, grab, serial); }
            }
            // Preserve the bridge's event order: newer physical events may
            // already have updated the compositor state while IBus replies.
            Request::Modifiers { mods_depressed, mods_latched, mods_locked, group } => {
                use smithay::input::keyboard::KeyboardTarget;
                if let Some(keyboard) = state.seat.get_keyboard() {
                    if let Some(focus) = keyboard.current_focus() {
                        let seat = state.seat.clone();
                        let mut modifiers = keyboard.modifier_state();
                        modifiers.serialized.depressed = mods_depressed;
                        modifiers.serialized.latched = mods_latched;
                        modifiers.serialized.locked = mods_locked;
                        modifiers.serialized.layout_effective = group;
                        focus.modifiers(&seat, state, modifiers, smithay::utils::SERIAL_COUNTER.next_serial());
                    }
                }
            },
            request => <smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState as Dispatch<_, _, State>>::request(
                state, client, resource, request, data, handle, data_init),
        }
    }
}

delegate_pointer_gestures!(State);
delegate_relative_pointer!(State);

impl PointerConstraintsHandler for State {
    /// Lock or confine at once when the surface already has the
    /// pointer; otherwise the window manager activates it on enter.
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
    }
}
delegate_pointer_constraints!(State);

impl XdgDialogHandler for State {}
delegate_xdg_dialog!(State);

impl XdgForeignHandler for State {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.protocols.foreign
    }
}
delegate_xdg_foreign!(State);

impl XdgSystemBellHandler for State {
    fn ring(&mut self, _surface: Option<WlSurface>) {
        self.protocols.bell.ring();
    }
}
delegate_xdg_system_bell!(State);

impl XdgToplevelTagHandler for State {
    fn set_tag(&mut self, toplevel: XdgToplevel, tag: String) {
        update_tag(self, &toplevel, |t| t.tag = Some(tag));
    }

    fn set_description(&mut self, toplevel: XdgToplevel, description: String) {
        update_tag(self, &toplevel, |t| t.description = Some(description));
    }
}
delegate_xdg_toplevel_tag!(State);

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.protocols.shortcuts_inhibit
    }

    /// Granted at once (GNOME Shell asks first; Roost has no such
    /// dialog). Super+Escape takes the shortcuts back.
    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.activate();
    }
}
smithay::delegate_keyboard_shortcuts_inhibit!(State);

impl GlobalDispatch<WpPointerWarpV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpPointerWarpV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WpPointerWarpV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpPointerWarpV1,
        request: wp_pointer_warp_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_pointer_warp_v1::Request::WarpPointer {
            surface,
            x,
            y,
            serial,
            ..
        } = request
        {
            state
                .protocols
                .pointer_warps
                .push((surface, (x, y).into(), serial));
        }
    }
}

/// A text cursor rectangle in a surface's coordinates, placed globally
/// by the surface's buffer origin.
fn global_cursor_rect(
    origin: Point<i32, Logical>,
    local: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    Rectangle::new(origin + local.loc, local.size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_hears_the_text_cursor_in_global_coordinates() {
        // A window whose surface starts at (300, 200), caret 40 px in
        // and 12 px down, 1 px wide and 18 tall.
        let local = Rectangle::new((40, 12).into(), (1, 18).into());
        let global = global_cursor_rect((300, 200).into(), local);
        assert_eq!(global, Rectangle::new((340, 212).into(), (1, 18).into()));
    }
}
