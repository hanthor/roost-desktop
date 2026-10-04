//! Headless compositor core for the 001 nested slice.
//!
//! This crate owns the Smithay `Display` plus the protocol states the
//! slice needs first (compositor, shm, xdg-shell, layer-shell,
//! data-device). Rendering
//! backends, seats, and the shell control channel arrive in later steps;
//! the [`TestCompositor`] helper drives the display manually so
//! integration tests stay deterministic without an event loop or GPU.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

#[cfg(feature = "xwayland")]
use smithay::delegate_xwayland_shell;
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_output, delegate_primary_selection,
    delegate_seat, delegate_shm, delegate_xdg_shell,
    input::{Seat, SeatHandler, SeatState},
    output::Output,
    reexports::wayland_server::{
        backend::{ClientData, ClientId, DisconnectReason, GlobalId},
        protocol::{wl_buffer, wl_output, wl_seat, wl_surface},
        Client, Display, DisplayHandle, Resource,
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            SelectionHandler,
        },
        shell::{
            wlr_layer::WlrLayerShellState,
            xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        },
        shm::{ShmHandler, ShmState},
    },
};

pub mod animation;
pub mod control;
#[cfg(feature = "drm")]
pub mod drm;
pub mod frame_timing;
pub mod idle_monitor;
pub mod ime;
pub mod introspect;
pub mod layer;
pub mod lock;
pub mod monitors;
pub mod mutter;
pub mod overlay;
pub mod overview;
pub mod pam;
pub mod popup;
pub mod protocols;
pub mod runtime;
pub mod screencast;
pub mod screenshot;
pub mod session_lock;
pub mod session_services;
#[cfg(feature = "drm")]
pub mod sleep;
pub mod spring;
pub mod state;
pub mod supervise;
pub mod unlock;
pub mod wallpaper;
pub mod windows;
pub mod xwayland;

/// One compositor-tracked output: protocol handle plus geometry.
/// The first entry is the primary output; single-output sessions hold
/// exactly one entry, which is today's path generalized, not a fork.
#[derive(Debug, Clone)]
pub(crate) struct OutputEntry {
    /// Protocol handle; `None` for virtual entries in tests (no
    /// display exists there to advertise a global on).
    pub output: Option<Output>,
    /// Registry global id from `create_global`; `None` for virtual
    /// entries. Removal un-advertises it so clients see the hotplug.
    pub global: Option<GlobalId>,
    /// Output name, e.g. `roost-0`.
    pub name: String,
    /// Output geometry layer surfaces and windows arrange against.
    pub size: smithay::utils::Size<i32, smithay::utils::Logical>,
    /// Top-left corner in the global compositor space: entries tile
    /// left to right, so a second output starts where the first ends.
    pub loc: (i32, i32),
    /// Whether the dock anchors here.
    pub primary: bool,
}

/// Compositor dispatch state: protocol states plus their handlers.
pub struct State {
    compositor_state: CompositorState,
    shm_state: ShmState,
    xdg_shell_state: XdgShellState,
    // Held alive for the output globals; never read directly.
    _output_manager_state: OutputManagerState,
    seat_state: SeatState<State>,
    // Kept alive for the seat global; input routing (R4) attaches here later.
    #[allow(dead_code)]
    seat: Seat<State>,
    /// Display handle for client lookups that need no round-trip
    /// (clipboard focus follows keyboard focus by resolving the
    /// focused surface to its client).
    dh: DisplayHandle,
    pub(crate) layer_shell_state: WlrLayerShellState,
    /// Clipboard/drag-and-drop manager (toolkit clients such as GTK
    /// and Chromium refuse a display without this global).
    data_device_state: DataDeviceState,
    /// The icon a client's drag-and-drop carries, drawn at the pointer
    /// until the drop.
    dnd_icon: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    /// Middle-click primary selection beside the clipboard.
    primary_selection_state: PrimarySelectionState,
    pub(crate) panel_surfaces: Vec<layer::PanelSurface>,
    /// `wl_surface`s whose layer-shell role was destroyed. Smithay 0.7
    /// keeps validating them on commit against reset (unanchored, zero
    /// size) state and kills the client; see [`State::new_surface`].
    pub(crate) dead_layer_surfaces: std::collections::HashSet<wl_surface::WlSurface>,
    /// Popup trees per parent surface (#88).
    pub(crate) popups: smithay::desktop::PopupManager,
    /// Popups holding an explicit grab, oldest first: an outside click
    /// dismisses all of them; keyboard focus sits on the newest.
    pub(crate) popup_grab: Vec<PopupSurface>,
    /// Set when the last grabbed popup went away, so the manager hands
    /// keyboard focus back to the focused window on its next reconcile.
    pub(crate) popup_refocus: bool,
    /// Global origin of each mapped toplevel's surface, published by the
    /// window manager every reconcile, so popups can be constrained to
    /// the output under their parent before their first configure.
    pub(crate) window_origins: std::collections::HashMap<
        wl_surface::WlSurface,
        smithay::utils::Point<i32, smithay::utils::Logical>,
    >,
    /// Output inventory: one entry per connected output, fed by the
    /// standard global add/remove flow. Empty until the runtime (or a
    /// test) registers the first entry; readers fall back to zero
    /// sizes, which configure zero sizes.
    pub(crate) outputs: Vec<OutputEntry>,
    /// Target remembered when the first preferred scale is sent, before mapping.
    pub(crate) initial_outputs: std::collections::HashMap<wl_surface::WlSurface, String>,
    /// Client window-state requests awaiting the manager's next
    /// `reconcile` drain (002 window actions).
    pub(crate) window_requests: Vec<(wl_surface::WlSurface, WindowRequest)>,
    /// dmabuf, activation, viewporter and the other #89 protocols.
    pub(crate) protocols: protocols::Protocols,
    /// presentation-time, fifo and commit-timing (#89).
    pub(crate) frame_timing: frame_timing::FrameTiming,
    /// ext-session-lock-v1: the shell's lock screen surfaces.
    pub(crate) lock_protocol: session_lock::LockProtocol,
    /// Running X11 window manager, once the compatibility server is
    /// up (xwayland feature only).
    #[cfg(feature = "xwayland")]
    pub(crate) xwm: Option<smithay::xwayland::X11Wm>,
    /// X11 manager events queued by the WM handlers for the next
    /// `reconcile` drain (xwayland feature only).
    #[cfg(feature = "xwayland")]
    pub(crate) x11_events: Vec<crate::xwayland::X11ManagerEvent>,
    /// XWayland shell global backing X11 surface association
    /// (xwayland feature only; kept alive for the global).
    #[cfg(feature = "xwayland")]
    pub(crate) xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState,
}

/// Per-client data: the compositor state slice each client sees.
#[derive(Default)]
pub(crate) struct ClientState {
    compositor_state: CompositorClientState,
    /// The IBus bridge the compositor spawned on a private socket: the
    /// one client allowed a virtual keyboard (to hand back the keys
    /// IBus does not take).
    pub(crate) ime_bridge: bool,
}

impl ClientState {
    /// State for the compositor's own IBus bridge.
    pub(crate) fn ime_bridge() -> Self {
        Self {
            ime_bridge: true,
            ..Self::default()
        }
    }
}

/// Client-initiated window state request (002 window actions).
/// The xdg-shell handlers only queue these on [`State`]; the window
/// manager drains and applies them once per tick in `reconcile`, so
/// protocol input and scene mutation stay on one call path for live
/// events and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowRequest {
    /// Client asked to be maximized.
    Maximize,
    /// Client asked to leave maximized.
    Unmaximize,
    /// Client asked to cover the output.
    Fullscreen,
    /// Client asked to leave fullscreen.
    Unfullscreen,
    /// Client started an interactive move (header-bar drag, #58).
    Move,
    /// Client started an interactive resize from these xdg edges.
    Resize(u32),
    /// A valid xdg-activation request: focus and raise (#89).
    Activate,
    /// Client asked to be minimized (GNOME's Hide).
    Minimize,
    /// Client asked for the window menu at this point of its window
    /// geometry (a right click on a header bar).
    Menu(i32, i32),
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, _surface: ToplevelSurface) {}

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        self.refresh_initial_surface_scale(surface.wl_surface());
    }

    /// Track the popup and place it where its positioner asks; the
    /// initial configure goes out on its first commit.
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        let wl_surface = surface.wl_surface().clone();
        let _ = self
            .popups
            .track_popup(smithay::desktop::PopupKind::Xdg(surface));
        self.refresh_initial_surface_scale(&wl_surface);
    }

    /// Explicit grab (menus): keyboard focus moves to the popup and an
    /// outside click dismisses the whole grabbed chain.
    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        let target = surface.wl_surface().clone();
        self.popup_grab.push(surface);
        // A new grab supersedes a pending hand-back from a popup that
        // closed earlier in the same tick (menu to menu on the panel).
        self.popup_refocus = false;
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(target), serial);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
        let _ = surface.send_configure();
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        let before = self.popup_grab.len();
        self.popup_grab
            .retain(|p| p.wl_surface() != surface.wl_surface());
        if before > 0 && self.popup_grab.is_empty() {
            self.popup_refocus = true;
        }
    }

    /// Queue an interactive move (CSD header-bar drag) for the manager.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        self.window_requests
            .push((surface.wl_surface().clone(), WindowRequest::Move));
    }

    /// Queue an interactive resize from `edges` for the manager.
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        self.window_requests.push((
            surface.wl_surface().clone(),
            WindowRequest::Resize(edges as u32),
        ));
    }

    /// Queue a client maximize request for the manager drain. The
    /// bare `send_configure` default would leave the client hanging
    /// in a state the scene never applies.
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.window_requests
            .push((surface.wl_surface().clone(), WindowRequest::Maximize));
    }

    /// Queue a client unmaximize request for the manager drain.
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.window_requests
            .push((surface.wl_surface().clone(), WindowRequest::Unmaximize));
    }

    /// Queue a client fullscreen request for the manager drain.
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<wl_output::WlOutput>,
    ) {
        self.window_requests
            .push((surface.wl_surface().clone(), WindowRequest::Fullscreen));
    }

    /// Queue a client unfullscreen request for the manager drain.
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.window_requests
            .push((surface.wl_surface().clone(), WindowRequest::Unfullscreen));
    }

    /// Queue a client minimize request for the manager drain.
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.window_requests
            .push((surface.wl_surface().clone(), WindowRequest::Minimize));
    }

    /// Queue a window-menu request (header-bar right click) for the
    /// manager drain; the shell draws GNOME's window menu there.
    fn show_window_menu(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        location: smithay::utils::Point<i32, smithay::utils::Logical>,
    ) {
        self.window_requests.push((
            surface.wl_surface().clone(),
            WindowRequest::Menu(location.x, location.y),
        ));
    }
}

impl State {
    /// Drain queued client window-state requests (the manager calls
    /// this from `reconcile`).
    pub(crate) fn take_window_requests(&mut self) -> Vec<(wl_surface::WlSurface, WindowRequest)> {
        std::mem::take(&mut self.window_requests)
    }

    /// Drain queued X11 manager events (the manager calls this from
    /// `reconcile`; xwayland feature only).
    #[cfg(feature = "xwayland")]
    pub(crate) fn take_x11_events(&mut self) -> Vec<crate::xwayland::X11ManagerEvent> {
        std::mem::take(&mut self.x11_events)
    }
}

impl CompositorHandler for State {
    fn destroyed(&mut self, surface: &wl_surface::WlSurface) {
        self.initial_outputs.remove(surface);
    }

    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Normal clients carry our `ClientState`. With the xwayland
        // feature the XWayland server client instead carries
        // Smithay's `XWaylandClientData`, which holds the same
        // compositor state under its own field.
        if let Some(data) = client.get_data::<ClientState>() {
            return &data.compositor_state;
        }
        #[cfg(feature = "xwayland")]
        if let Some(data) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &data.compositor_state;
        }
        panic!("client without compositor state");
    }

    /// Work around a Smithay 0.7 layer-shell bug. When a client destroys
    /// a `zwlr_layer_surface_v1`, Smithay resets the surface's layer
    /// state to defaults (no anchors, zero size) but leaves its commit
    /// validator registered; the next commit on that `wl_surface` (GTK
    /// commits a null buffer right after destroying the role) then posts
    /// "width 0 requested without setting left and right anchors" and
    /// disconnects the client. This hook is registered when the surface
    /// is created, so it runs before Smithay's: for a surface whose layer
    /// role is gone it marks the meaningless pending state as anchored on
    /// every edge, which the validator accepts.
    fn new_surface(&mut self, surface: &wl_surface::WlSurface) {
        smithay::wayland::compositor::add_pre_commit_hook::<State, _>(
            surface,
            |state, _dh, surface| {
                if state.dead_layer_surfaces.contains(surface) {
                    smithay::wayland::compositor::with_states(surface, |states| {
                        let mut cached = states
                            .cached_state
                            .get::<smithay::wayland::shell::wlr_layer::LayerSurfaceCachedState>();
                        cached.pending().anchor = smithay::wayland::shell::wlr_layer::Anchor::all();
                    });
                }
            },
        );
    }

    fn commit(&mut self, surface: &wl_surface::WlSurface) {
        on_commit_buffer_handler::<State>(surface);
        self.popups.commit(surface);
        if let Some(smithay::desktop::PopupKind::Xdg(popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                self.unconstrain_popup(&popup);
                let _ = popup.send_configure();
            }
        }
        crate::layer::arrange_after_commit(self);
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl OutputHandler for State {}

/// Clipboard/drag-and-drop: advertised so toolkit clients accept the
/// display. Client copy/paste and primary selection round-trip
/// through the smithay selection state; both focuses mirror keyboard
/// focus (see [`State::sync_selection_focus`]). Server-initiated
/// selection content and drag-and-drop pointer grabs are not served
/// yet.
impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        _source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>,
        icon: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.dnd_icon = icon;
    }

    fn dropped(
        &mut self,
        _target: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
        _validated: bool,
        _seat: Seat<Self>,
    ) {
        self.dnd_icon = None;
    }
}

impl State {
    /// The icon of the drag in progress, if any (and still alive).
    pub fn dnd_icon(
        &self,
    ) -> Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
        use smithay::reexports::wayland_server::Resource;
        self.dnd_icon.clone().filter(|s| s.is_alive())
    }
}
impl ServerDndGrabHandler for State {}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl SeatHandler for State {
    type KeyboardFocus = wl_surface::WlSurface;
    type PointerFocus = wl_surface::WlSurface;
    type TouchFocus = wl_surface::WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    /// Keyboard focus moved: a shortcuts inhibitor suspended with
    /// Super+Escape applies again once its surface is focused anew, as
    /// in Mutter (#89).
    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&wl_surface::WlSurface>) {
        if let Some(surface) = focused {
            self.reactivate_inhibitor(surface);
        }
    }
}

/// Wayland seat name shared by the protocol state, the token store's
/// seat binding, and the control hub.
pub const SEAT_NAME: &str = "roost-seat";

impl State {
    /// Fit a popup onto the output under its parent using the client's
    /// positioner constraint adjustments (flip, slide, resize in protocol
    /// order). Parents are toplevels (positioned relative to their window
    /// geometry) or layer surfaces (relative to the surface); nested
    /// popups keep their positioner geometry.
    pub(crate) fn unconstrain_popup(&self, popup: &PopupSurface) {
        use smithay::utils::{Logical, Point, Rectangle};
        let Some(parent) = popup.get_parent_surface() else {
            return;
        };
        let base: Point<i32, Logical> = if let Some(window_loc) = self.window_origins.get(&parent) {
            // Positioners are relative to the parent's window geometry,
            // which is exactly the window rect the manager tracks.
            *window_loc
        } else if let Some((_, (x, y), _)) = crate::layer::layer_layout(self)
            .into_iter()
            .find(|(s, _, _)| *s == parent)
        {
            (x, y).into()
        } else {
            return;
        };
        let output = self
            .outputs
            .iter()
            .map(|e| Rectangle::<i32, Logical>::new(e.loc.into(), e.size))
            .find(|r| r.contains(base))
            .or_else(|| {
                self.outputs
                    .iter()
                    .find(|e| e.primary)
                    .map(|e| Rectangle::new(e.loc.into(), e.size))
            });
        let Some(output) = output else {
            return;
        };
        let target = Rectangle::new(output.loc - base, output.size);
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }

    /// Dismiss every grabbed popup (outside click): `popup_done` to each,
    /// newest first. Returns whether anything was dismissed.
    pub(crate) fn dismiss_popup_grab(&mut self) -> bool {
        if self.popup_grab.is_empty() {
            return false;
        }
        for popup in self.popup_grab.drain(..).rev() {
            popup.send_popup_done();
        }
        self.popup_refocus = true;
        true
    }

    /// Take the "hand keyboard focus back" flag (see `popup_refocus`).
    pub(crate) fn take_popup_refocus(&mut self) -> bool {
        std::mem::take(&mut self.popup_refocus)
    }

    /// Client that owns the active popup grab, if any.
    pub(crate) fn popup_grab_owner(
        &self,
    ) -> Option<smithay::reexports::wayland_server::backend::ClientId> {
        self.popup_grab
            .first()
            .and_then(|p| p.wl_surface().client())
            .map(|c| c.id())
    }

    /// Whether an explicit popup grab is active.
    pub fn popup_grab_active(&self) -> bool {
        !self.popup_grab.is_empty()
    }

    /// Seat for capability attachment and input routing.
    pub(crate) fn seat_mut(&mut self) -> &mut Seat<State> {
        &mut self.seat
    }

    /// Mirror keyboard focus into selection focus: the newly focused
    /// client's data device and primary device receive the current
    /// offers, if any. `None` clears both. Surfaces from gone clients
    /// resolve to no client, which also clears.
    pub(crate) fn sync_selection_focus(&mut self, surface: Option<&wl_surface::WlSurface>) {
        let client = surface.and_then(|s| self.dh.get_client(s.id()).ok());
        set_data_device_focus(&self.dh, &self.seat, client.clone());
        set_primary_focus(&self.dh, &self.seat, client);
    }

    /// Number of currently mapped toplevel surfaces.
    pub fn toplevel_count(&self) -> usize {
        self.xdg_shell_state.toplevel_surfaces().len()
    }

    /// Currently mapped toplevel surfaces, for frame production.
    pub fn toplevels(&self) -> Vec<ToplevelSurface> {
        self.xdg_shell_state.toplevel_surfaces().to_vec()
    }

    /// The first mapped toplevel's Wayland surface, if any.
    pub fn first_toplevel_surface(
        &self,
    ) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
        self.xdg_shell_state.toplevel_surfaces()[0]
            .wl_surface()
            .clone()
    }
}

delegate_xdg_shell!(State);
#[cfg(feature = "xwayland")]
delegate_xwayland_shell!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_compositor!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_output!(State);

/// A manually-driven compositor for tests: no calloop loop, no backend.
pub struct TestCompositor {
    pub display: Display<State>,
    pub state: State,
}

impl Default for TestCompositor {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    /// Protocol state shared by the headless test helper and the nested
    /// runtime: compositor, shm, xdg-shell, layer-shell, seat, and output
    /// globals.
    pub fn new(dh: &DisplayHandle) -> Self {
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(dh, SEAT_NAME);
        State {
            // v6 (GNOME 51's): preferred buffer scale goes out per surface
            // (`send_surface_scales` in the runtime).
            compositor_state: CompositorState::new_v6::<State>(dh),
            shm_state: ShmState::new::<State>(dh, vec![]),
            xdg_shell_state: XdgShellState::new::<State>(dh),
            _output_manager_state: OutputManagerState::new_with_xdg_output::<State>(dh),
            seat_state,
            seat,
            dh: dh.clone(),
            layer_shell_state: WlrLayerShellState::new::<State>(dh),
            data_device_state: DataDeviceState::new::<State>(dh),
            dnd_icon: None,
            primary_selection_state: PrimarySelectionState::new::<State>(dh),
            panel_surfaces: Vec::new(),
            dead_layer_surfaces: std::collections::HashSet::new(),
            popups: smithay::desktop::PopupManager::default(),
            popup_grab: Vec::new(),
            popup_refocus: false,
            window_origins: std::collections::HashMap::new(),
            initial_outputs: std::collections::HashMap::new(),
            outputs: Vec::new(),
            window_requests: Vec::new(),
            protocols: protocols::Protocols::new(dh),
            frame_timing: frame_timing::FrameTiming::new(dh),
            lock_protocol: session_lock::LockProtocol::new(dh),
            #[cfg(feature = "xwayland")]
            xwm: None,
            #[cfg(feature = "xwayland")]
            x11_events: Vec::new(),
            #[cfg(feature = "xwayland")]
            xwayland_shell_state: smithay::wayland::xwayland_shell::XWaylandShellState::new::<State>(
                dh,
            ),
        }
    }

    /// Set the primary output geometry for layer-surface arrange
    /// (tests call this with their fixture size at setup). With no
    /// entries yet this registers a virtual primary entry, so the
    /// single-output path stays the one-entry case.
    pub fn set_output_size(&mut self, width: i32, height: i32) {
        let size = smithay::utils::Size::from((width, height));
        match self.outputs.iter_mut().find(|entry| entry.primary) {
            Some(entry) => entry.size = size,
            None => self.outputs.push(OutputEntry {
                output: None,
                global: None,
                name: "roost-0".to_owned(),
                size,
                loc: (0, 0),
                primary: true,
            }),
        }
    }

    /// Register an output in the inventory (upsert by name): the
    /// runtime calls this once per output at launch and on hotplug
    /// add. The first entry registered becomes primary; later entries
    /// tile to the right of the existing row, so every output owns a
    /// distinct slice of the global space with no overlap.
    pub fn add_output(&mut self, name: &str, output: Option<Output>, width: i32, height: i32) {
        let size = smithay::utils::Size::from((width, height));
        if let Some(entry) = self.outputs.iter_mut().find(|entry| entry.name == name) {
            entry.output = output;
            entry.size = size;
            return;
        }
        let primary = self.outputs.is_empty();
        let x = self
            .outputs
            .iter()
            .map(|entry| entry.loc.0 + entry.size.w.max(0))
            .max()
            .unwrap_or(0);
        self.outputs.push(OutputEntry {
            output,
            global: None,
            name: name.to_owned(),
            size,
            loc: (x, 0),
            primary,
        });
    }

    /// Place a registered output at a logical position (GNOME's
    /// arrangement from monitors.xml, #59). Unknown names are ignored.
    pub fn set_output_location(&mut self, name: &str, loc: (i32, i32)) {
        if let Some(entry) = self.outputs.iter_mut().find(|entry| entry.name == name) {
            entry.loc = loc;
        }
    }

    /// Record the registry global id for a registered output (from
    /// `create_global`): removal un-advertises it. Unknown names are
    /// ignored — virtual entries never advertise.
    pub fn note_output_global(&mut self, name: &str, id: GlobalId) {
        if let Some(entry) = self.outputs.iter_mut().find(|entry| entry.name == name) {
            entry.global = Some(id);
        }
    }

    /// Drop an output from the inventory; a surviving first entry
    /// takes over primary. Callers migrate windows first (see
    /// [`WindowManager::migrate_output_windows`](crate::windows::WindowManager::migrate_output_windows)),
    /// so no window is stranded, then re-apply derived layouts when
    /// the primary changed (see
    /// [`WindowManager::reapply_derived_layouts`](crate::windows::WindowManager::reapply_derived_layouts)).
    /// Real entries un-advertise their global, so clients see the
    /// hotplug removal live; virtual entries just drop. Returns
    /// whether an entry was removed.
    pub fn remove_output(&mut self, name: &str) -> bool {
        let Some(index) = self.outputs.iter().position(|entry| entry.name == name) else {
            return false;
        };
        let was_primary = self.outputs[index].primary;
        let global = self.outputs[index].global.take();
        if let Some(id) = global {
            self.dh.disable_global::<State>(id.clone());
            self.dh.remove_global::<State>(id);
        }
        self.outputs.remove(index);
        if was_primary {
            if let Some(first) = self.outputs.first_mut() {
                first.primary = true;
            }
        }
        true
    }

    /// Move the dock anchor: mark `name` primary, clearing the flag
    /// elsewhere. Returns whether the entry exists.
    pub fn set_primary(&mut self, name: &str) -> bool {
        if !self.outputs.iter().any(|entry| entry.name == name) {
            return false;
        }
        for entry in &mut self.outputs {
            entry.primary = entry.name == name;
        }
        true
    }

    /// Geometry readers arrange against: the primary entry's size,
    /// or zero when the inventory is empty.
    pub fn primary_size(&self) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        self.outputs
            .iter()
            .find(|entry| entry.primary)
            .map(|entry| entry.size)
            .unwrap_or_default()
    }

    /// Geometry for one named output (layer surfaces bound to it), or
    /// the primary entry's when the name is unknown or unbound: an
    /// unbound surface behaves like today's global one.
    pub fn size_for_output(
        &self,
        name: Option<&str>,
    ) -> smithay::utils::Size<i32, smithay::utils::Logical> {
        name.and_then(|name| {
            self.outputs
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.size)
        })
        .unwrap_or_else(|| self.primary_size())
    }

    /// Top-left corner of one named output in the global space, or the
    /// primary entry's when unknown: placement falls back to the
    /// primary slice, matching [`size_for_output`](Self::size_for_output).
    pub fn loc_for_output(&self, name: Option<&str>) -> (i32, i32) {
        name.and_then(|name| {
            self.outputs
                .iter()
                .find(|entry| entry.name == name)
                .map(|entry| entry.loc)
        })
        .unwrap_or_else(|| {
            self.outputs
                .iter()
                .find(|entry| entry.primary)
                .map(|entry| entry.loc)
                .unwrap_or((0, 0))
        })
    }

    /// Wire records for the shell, primary first: the inventory as the
    /// shell already knows how to paint it — size, primary, handles
    /// implied by the bound surfaces.
    /// Output chosen for a new window: pointer, keyboard focus, primary.
    /// Placement and the first fractional-scale event use the same policy.
    pub(crate) fn new_window_output(&self) -> Option<&OutputEntry> {
        let pointer = self.seat.get_pointer().map(|p| p.current_location());
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|k| k.current_focus())
            .and_then(|s| self.window_origins.get(&s).copied());
        self.outputs
            .iter()
            .find(|entry| {
                pointer.is_some_and(|p| {
                    smithay::utils::Rectangle::new(entry.loc.into(), entry.size)
                        .to_f64()
                        .contains(p)
                })
            })
            .or_else(|| {
                self.outputs.iter().find(|entry| {
                    focused.is_some_and(|p| {
                        smithay::utils::Rectangle::new(entry.loc.into(), entry.size).contains(p)
                    })
                })
            })
            .or_else(|| self.outputs.iter().find(|entry| entry.primary))
    }

    /// First preferred scale before a new window has committed a buffer.
    pub fn initial_window_scale(&self) -> f64 {
        self.new_window_output()
            .and_then(|entry| entry.output.as_ref())
            .map(|output| output.current_scale().fractional_scale())
            .unwrap_or(self.protocols.preferred_scale)
    }

    /// Scale of the output a logical rect overlaps most (the primary on
    /// a tie or no overlap), as niri picks it for a window (#59).
    pub fn scale_for(
        &self,
        rect: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
    ) -> smithay::output::Scale {
        let overlap = |entry: &OutputEntry| {
            let area = smithay::utils::Rectangle::new(entry.loc.into(), entry.size);
            area.intersection(rect).map_or(0, |i| i.size.w * i.size.h)
        };
        self.outputs
            .iter()
            .filter(|entry| entry.output.is_some())
            .max_by_key(|entry| (overlap(entry), entry.primary))
            .and_then(|entry| entry.output.as_ref())
            .map(|output| output.current_scale())
            .unwrap_or(smithay::output::Scale::Integer(1))
    }

    /// Every registered output with its protocol object: name, output,
    /// logical position, primary flag.
    /// The primary output's name.
    pub fn primary_output_name(&self) -> Option<String> {
        self.outputs
            .iter()
            .find(|e| e.primary)
            .or_else(|| self.outputs.first())
            .map(|e| e.name.clone())
    }

    pub fn output_entries(&self) -> Vec<(String, Output, (i32, i32), bool)> {
        self.outputs
            .iter()
            .filter_map(|e| Some((e.name.clone(), e.output.clone()?, e.loc, e.primary)))
            .collect()
    }

    /// Shell-facing output inventory.
    pub fn output_infos(&self) -> Vec<roost_shell_control::OutputInfo> {
        let mut entries: Vec<&OutputEntry> = self.outputs.iter().collect();
        entries.sort_by_key(|entry| (!entry.primary, entry.loc.0));
        entries
            .into_iter()
            .map(|entry| roost_shell_control::OutputInfo {
                name: entry.name.clone(),
                width: entry.size.w,
                height: entry.size.h,
                primary: entry.primary,
            })
            .collect()
    }

    /// Protocol handle of the primary output, for frame callbacks.
    pub fn primary_output(&self) -> Option<Output> {
        self.outputs
            .iter()
            .find(|entry| entry.primary)
            .and_then(|entry| entry.output.clone())
    }
}

impl TestCompositor {
    /// Create the display and advertise compositor, shm, xdg-shell,
    /// layer-shell, and the data-device manager.
    pub fn new() -> Self {
        let display: Display<State> = Display::new().unwrap();
        let dh = display.handle();
        let state = State::new(&dh);
        Self { display, state }
    }

    /// Attach a client connected through `stream`.
    pub fn add_client(&mut self, stream: UnixStream) -> Client {
        self.display
            .handle()
            .insert_client(stream, Arc::new(ClientState::default()))
            .unwrap()
    }

    /// Dispatch pending client requests and flush replies. Drives the
    /// display without an event loop; tests interleave this with client
    /// actions instead of blocking on either side.
    pub fn pump(&mut self) {
        self.display.dispatch_clients(&mut self.state).unwrap();
        self.display.flush_clients().unwrap();
    }

    /// Build a [`windows::WindowManager`] attached to the test seat, for
    /// headless mapping/input tests without a backend.
    pub fn window_manager(&mut self) -> windows::WindowManager {
        windows::WindowManager::new(&mut self.state)
    }
}
