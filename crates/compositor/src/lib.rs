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

pub mod control;
#[cfg(feature = "drm")]
pub mod drm;
pub mod layer;
pub mod lock;
pub mod overlay;
pub mod overview;
pub mod popup;
pub mod runtime;
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
    /// Middle-click primary selection beside the clipboard.
    primary_selection_state: PrimarySelectionState,
    pub(crate) panel_surfaces: Vec<layer::PanelSurface>,
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
    /// Client window-state requests awaiting the manager's next
    /// `reconcile` drain (002 window actions).
    pub(crate) window_requests: Vec<(wl_surface::WlSurface, WindowRequest)>,
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

    /// Track the popup and place it where its positioner asks; the
    /// initial configure goes out on its first commit.
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        let _ = self
            .popups
            .track_popup(smithay::desktop::PopupKind::Xdg(surface));
    }

    /// Explicit grab (menus): keyboard focus moves to the popup and an
    /// outside click dismisses the whole grabbed chain.
    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        let target = surface.wl_surface().clone();
        self.popup_grab.push(surface);
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
            popups: smithay::desktop::PopupManager::default(),
            popup_grab: Vec::new(),
            popup_refocus: false,
            window_origins: std::collections::HashMap::new(),
            outputs: Vec::new(),
            window_requests: Vec::new(),
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
