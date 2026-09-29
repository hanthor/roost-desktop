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

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_output, delegate_primary_selection,
    delegate_seat, delegate_shm, delegate_xdg_shell,
    input::{Seat, SeatHandler, SeatState},
    reexports::wayland_server::{
        backend::{ClientData, ClientId, DisconnectReason},
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

pub mod control;
pub mod layer;
pub mod overlay;
pub mod runtime;
pub mod state;
pub mod supervise;
pub mod wallpaper;
pub mod windows;

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
    /// Middle-click primary selection beside the clipboard.
    primary_selection_state: PrimarySelectionState,
    pub(crate) panel_surfaces: Vec<layer::PanelSurface>,
    /// Output geometry layer surfaces arrange against (set by the
    /// runtime; defaults to zero, which configures zero sizes).
    pub(crate) output_size: smithay::utils::Size<i32, smithay::utils::Logical>,
    /// Client window-state requests awaiting the manager's next
    /// `reconcile` drain (002 window actions).
    pub(crate) window_requests: Vec<(wl_surface::WlSurface, WindowRequest)>,
}

/// Per-client data: the compositor state slice each client sees.
#[derive(Default)]
pub(crate) struct ClientState {
    compositor_state: CompositorClientState,
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

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
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
}

impl State {
    /// Drain queued client window-state requests (the manager calls
    /// this from `reconcile`).
    pub(crate) fn take_window_requests(&mut self) -> Vec<(wl_surface::WlSurface, WindowRequest)> {
        std::mem::take(&mut self.window_requests)
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &wl_surface::WlSurface) {
        on_commit_buffer_handler::<State>(surface);
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

impl ClientDndGrabHandler for State {}
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&wl_surface::WlSurface>) {}
}

/// Wayland seat name shared by the protocol state, the token store's
/// seat binding, and the control hub.
pub const SEAT_NAME: &str = "roost-seat";

impl State {
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
            compositor_state: CompositorState::new::<State>(dh),
            shm_state: ShmState::new::<State>(dh, vec![]),
            xdg_shell_state: XdgShellState::new::<State>(dh),
            _output_manager_state: OutputManagerState::new_with_xdg_output::<State>(dh),
            seat_state,
            seat,
            dh: dh.clone(),
            layer_shell_state: WlrLayerShellState::new::<State>(dh),
            data_device_state: DataDeviceState::new::<State>(dh),
            primary_selection_state: PrimarySelectionState::new::<State>(dh),
            panel_surfaces: Vec::new(),
            output_size: Default::default(),
            window_requests: Vec::new(),
        }
    }

    /// Set the output geometry for layer-surface arrange (the nested
    /// runtime calls this with its session size at launch).
    pub fn set_output_size(&mut self, width: i32, height: i32) {
        self.output_size = smithay::utils::Size::from((width, height));
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
