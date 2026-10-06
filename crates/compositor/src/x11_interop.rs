//! Trusted GNOME portal dialogs parented to managed X11 windows (#404).
//! Wire contract: libgxdp df896e3412b749947bc6f62a91a1aac8e6b6d19b.
use crate::{ClientState, State};
use smithay::reexports::wayland_server::{
    backend::GlobalId, protocol::wl_surface::WlSurface, Client, DataInit, Dispatch, DisplayHandle,
    GlobalDispatch, New, Resource,
};
use smithay::xwayland::X11Surface;
use std::collections::HashMap;

#[allow(dead_code, unused_imports, non_camel_case_types, clippy::all)]
pub mod server {
    use wayland_server;
    use wayland_server::protocol::wl_surface;
    pub mod __interfaces {
        use wayland_server::backend as wayland_backend;
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/mutter-x11-interop.xml");
    }
    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocols/mutter-x11-interop.xml");
}
use server::mutter_x11_interop::{self, MutterX11Interop};

pub(crate) struct Interop {
    _global: GlobalId,
    /// A live managed-window generation and its original X11 handle. Requests
    /// capture the model ID; an XID recycled after dispatch cannot reparent them.
    pub(crate) parents: HashMap<u32, (u64, X11Surface)>,
    pub(crate) requests: HashMap<WlSurface, u64>,
}
impl Interop {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: dh.create_global::<State, MutterX11Interop, ()>(1, ()),
            parents: HashMap::new(),
            requests: HashMap::new(),
        }
    }
}
fn authorized(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|data| data.x11_interop)
}
impl GlobalDispatch<MutterX11Interop, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        client: &Client,
        resource: New<MutterX11Interop>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        if authorized(client) {
            data_init.init(resource, ());
        } else {
            data_init.post_error(
                resource,
                0u32,
                "X11 interop requires typed service admission",
            );
        }
    }
    fn can_view(client: Client, _data: &()) -> bool {
        authorized(&client)
    }
}
impl Dispatch<MutterX11Interop, ()> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        _resource: &MutterX11Interop,
        request: mutter_x11_interop::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let mutter_x11_interop::Request::SetX11Parent { surface, xwindow } = request {
            if !authorized(client)
                || state.protocols.shortcut_locked
                || surface
                    .client()
                    .is_none_or(|owner| owner.id() != client.id())
                || !surface.is_alive()
                || !state.toplevels().iter().any(|t| t.wl_surface() == &surface)
            {
                return;
            }
            let Some((id, parent)) = state.protocols.x11_interop.parents.get(&xwindow) else {
                return;
            };
            if parent.alive() && !parent.is_override_redirect() {
                state.protocols.x11_interop.requests.insert(surface, *id);
            }
        }
    }
}
