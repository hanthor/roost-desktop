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
//! - viewporter, fractional-scale v1 (preferred scale 1 until #59),
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
//!   focused again. A trusted shell consent response is required first;
//!   unapproved requests remain inactive, even after refocusing.
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
    shortcut_approved: Vec<KeyboardShortcutsInhibitor>,
    shortcut_pending: Option<(u64, KeyboardShortcutsInhibitor, String, Instant)>,
    shortcut_next: u64,
    shortcut_changed: bool,
    shortcut_locked: bool,
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
            shortcut_approved: Vec::new(),
            shortcut_pending: None,
            shortcut_next: 0,
            shortcut_changed: false,
            shortcut_locked: false,
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
        if self.protocols.shortcut_locked {
            return false;
        }
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
        if self.protocols.shortcut_locked {
            return false;
        }
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
    pub(crate) fn reactivate_inhibitor(&mut self, surface: &WlSurface) {
        // A shell layer dialog may hold focus during consent. Switching
        // to a different app cancels it, so a late Allow cannot grant it.
        if self
            .toplevels()
            .iter()
            .any(|toplevel| toplevel.wl_surface() == surface)
            && self
                .protocols
                .shortcut_pending
                .as_ref()
                .is_some_and(|(_, i, _, _)| i.wl_surface() != surface)
        {
            self.cancel_shortcut_consent();
        }
        self.protocols.shortcut_approved.retain(|i| {
            i.wl_surface().is_alive()
                && self
                    .seat
                    .keyboard_shortcuts_inhibitor_for_surface(i.wl_surface())
                    .as_ref()
                    == Some(i)
        });
        for inhibitor in &self.protocols.shortcut_approved {
            if inhibitor.wl_surface() == surface && !self.protocols.shortcut_locked {
                if !inhibitor.is_active() {
                    inhibitor.activate();
                }
            } else if inhibitor.is_active() {
                inhibitor.inactivate();
            }
        }
    }

    fn cancel_shortcut_consent(&mut self) {
        if self.protocols.shortcut_pending.take().is_some() {
            self.protocols.shortcut_changed = true;
        }
    }

    /// Lock transitions fail closed and invalidate outstanding consent.
    pub fn set_shortcut_inhibition_locked(&mut self, locked: bool) {
        self.protocols.shortcut_locked = locked;
        if locked {
            self.cancel_shortcut_consent();
            for i in &self.protocols.shortcut_approved {
                if i.is_active() {
                    i.inactivate();
                }
            }
        }
    }

    /// Current request, useful to protocol proofs as well as the shell.
    pub fn shortcut_consent_request(&self) -> Option<(u64, String)> {
        self.protocols
            .shortcut_pending
            .as_ref()
            .map(|(id, _, app, _)| (*id, app.clone()))
    }

    /// Apply a trusted shell answer only to the live, mapped inhibitor
    /// that issued this request. Denied requests never reactivate on focus.
    pub fn answer_shortcut_consent(&mut self, request: u64, allow: bool) {
        if self.protocols.shortcut_locked
            || !self
                .protocols
                .shortcut_pending
                .as_ref()
                .is_some_and(|(id, _, _, _)| *id == request)
        {
            return;
        }
        let (_, inhibitor, _, since) = self.protocols.shortcut_pending.take().unwrap();
        self.protocols.shortcut_changed = true;
        if since.elapsed() > Duration::from_secs(30) {
            return;
        }
        if allow
            && self.window_origins.contains_key(inhibitor.wl_surface())
            && self
                .seat
                .keyboard_shortcuts_inhibitor_for_surface(inhibitor.wl_surface())
                .as_ref()
                == Some(&inhibitor)
        {
            if self
                .seat
                .get_keyboard()
                .and_then(|k| k.current_focus())
                .as_ref()
                == Some(inhibitor.wl_surface())
            {
                inhibitor.activate();
            }
            self.protocols.shortcut_approved.push(inhibitor);
        }
    }

    pub(crate) fn take_shortcut_consent_update(&mut self) -> Option<Option<(u64, String)>> {
        if self
            .protocols
            .shortcut_pending
            .as_ref()
            .is_some_and(|(_, i, _, since)| {
                since.elapsed() > Duration::from_secs(30)
                    || !self.window_origins.contains_key(i.wl_surface())
                    || self
                        .seat
                        .keyboard_shortcuts_inhibitor_for_surface(i.wl_surface())
                        .as_ref()
                        != Some(i)
            })
        {
            self.cancel_shortcut_consent();
        }
        std::mem::take(&mut self.protocols.shortcut_changed)
            .then(|| self.shortcut_consent_request())
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

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Clients render at the output scale (#59).
        let scale = self.protocols.preferred_scale;
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
smithay::delegate_virtual_keyboard_manager!(State);

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

    /// An unapproved inhibitor never becomes active, including on refocus.
    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        inhibitor.inactivate();
        if self.protocols.shortcut_locked
            || self.protocols.shortcut_pending.is_some()
            || !self.window_origins.contains_key(inhibitor.wl_surface())
            || self
                .seat
                .get_keyboard()
                .and_then(|k| k.current_focus())
                .as_ref()
                != Some(inhibitor.wl_surface())
        {
            return;
        }
        let app = with_states(inhibitor.wl_surface(), |states| {
            states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().app_id.clone())
                .unwrap_or_default()
        });
        self.protocols.shortcut_next += 1;
        self.protocols.shortcut_pending =
            Some((self.protocols.shortcut_next, inhibitor, app, Instant::now()));
        self.protocols.shortcut_changed = true;
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
