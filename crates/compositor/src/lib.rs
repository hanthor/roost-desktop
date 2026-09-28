//! Headless compositor core for the 001 nested slice.
//!
//! This crate owns the Smithay `Display` plus the protocol states the
//! slice needs first (compositor, shm, xdg-shell). Rendering backends,
//! seats, and the shell control channel arrive in later steps; the
//! [`TestCompositor`] helper drives the display manually so integration
//! tests stay deterministic without an event loop or GPU.

use std::os::unix::net::UnixStream;
use std::sync::Arc;

use smithay::{
    delegate_compositor, delegate_seat, delegate_shm, delegate_xdg_shell,
    input::{Seat, SeatHandler, SeatState},
    reexports::wayland_server::{
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer, wl_seat, wl_surface},
        Client, Display,
    },
    utils::Serial,
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
    },
};

pub mod control;
pub mod overlay;
pub mod state;
pub mod supervise;

/// Compositor dispatch state: protocol states plus their handlers.
pub struct State {
    compositor_state: CompositorState,
    shm_state: ShmState,
    xdg_shell_state: XdgShellState,
    seat_state: SeatState<State>,
    // Kept alive for the seat global; input routing (R4) attaches here later.
    #[allow(dead_code)]
    seat: Seat<State>,
}

/// Per-client data: the compositor state slice each client sees.
#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
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
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, _surface: &wl_surface::WlSurface) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
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

impl State {
    /// Number of currently mapped toplevel surfaces.
    pub fn toplevel_count(&self) -> usize {
        self.xdg_shell_state.toplevel_surfaces().len()
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
delegate_compositor!(State);
delegate_shm!(State);
delegate_seat!(State);

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

impl TestCompositor {
    /// Create the display and advertise compositor, shm, and xdg-shell.
    pub fn new() -> Self {
        let display: Display<State> = Display::new().unwrap();
        let dh = display.handle();
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(&dh, "test-seat");
        let state = State {
            compositor_state: CompositorState::new::<State>(&dh),
            shm_state: ShmState::new::<State>(&dh, vec![]),
            xdg_shell_state: XdgShellState::new::<State>(&dh),
            seat_state,
            seat,
        };
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
}
