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
//!   tracked like any other popup.
//! - pointer-gestures, relative-pointer and pointer-constraints (#60,
//!   #89): touchpad gestures reach apps, games get raw motion and a
//!   locked pointer.
//!
//! Deliberately absent, matching Mutter: xdg-decoration (GNOME is
//! client-side decorations only).

use std::time::Duration;

use smithay::{
    backend::allocator::{dmabuf::Dmabuf, Format},
    delegate_cursor_shape, delegate_dmabuf, delegate_fractional_scale, delegate_idle_inhibit,
    delegate_input_method_manager, delegate_pointer_constraints, delegate_pointer_gestures,
    delegate_relative_pointer, delegate_single_pixel_buffer, delegate_text_input_manager,
    delegate_viewporter, delegate_xdg_activation,
    input::pointer::PointerHandle,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle, Resource},
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
        pointer_constraints::{
            with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState,
        },
        pointer_gestures::PointerGesturesState,
        relative_pointer::RelativePointerManagerState,
        single_pixel_buffer::SinglePixelBufferState,
        tablet_manager::TabletSeatHandler,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
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
    /// Surfaces holding an idle inhibitor.
    pub(crate) inhibitors: Vec<WlSurface>,
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
            inhibitors: Vec::new(),
        }
    }
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

    /// Whether a live, mapped surface holds an idle inhibitor.
    pub fn idle_inhibited(&mut self) -> bool {
        self.protocols.inhibitors.retain(|s| s.is_alive());
        !self.protocols.inhibitors.is_empty()
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
        // Integer scale 1 until output scaling lands (#59).
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(1.0));
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

    fn popup_repositioned(&mut self, _surface: ImPopup) {}

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
