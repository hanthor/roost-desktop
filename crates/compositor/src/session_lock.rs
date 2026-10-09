//! ext-session-lock-v1: the shell's lock screen (GNOME 51's curtain and
//! unlock prompt) as lock surfaces. Adapted from niri's handler
//! (GPL-3.0-or-later, like Tuna Desktop).
//!
//! Authority stays with the compositor (ADR: the lock flag is
//! compositor-owned). A lock request is granted only to the supervised
//! shell (the runtime checks the client's pid), and a client's unlock
//! never unlocks by itself: the session unlocks only once the
//! compositor has verified the password (PAM or greetd) and cleared its
//! flag. Until then the lock surfaces are all that is drawn and all that
//! takes input.

use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
    ext_session_lock_v1::{Error, ExtSessionLockV1, Request},
};
use smithay::reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use smithay::reexports::wayland_server::{
    backend::ClientId, Client, DataInit, Dispatch, DisplayHandle, Resource,
};
use smithay::wayland::session_lock::{
    ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
    SessionLockManagerState, SessionLockState, SessionLocker,
};

use crate::State;

/// Session-lock protocol state the handler and the runtime share.
pub(crate) struct LockProtocol {
    pub(crate) state: SessionLockManagerState,
    /// A lock request awaiting the runtime's decision, with the
    /// requesting client's pid.
    pub(crate) pending: Option<(SessionLocker, Option<i32>)>,
    /// Only this runtime-approved client may supply visible lock surfaces.
    pub(crate) owner: Option<ClientId>,
    /// Only the approved lock resource may request release.
    approved_lock: Option<ExtSessionLockV1>,
    /// Lock surfaces by output name.
    pub(crate) surfaces: Vec<(LockSurface, String)>,
    /// The lock client asked to unlock (unlock_and_destroy).
    pub(crate) client_unlocked: bool,
}

impl LockProtocol {
    pub(crate) fn new(dh: &smithay::reexports::wayland_server::DisplayHandle) -> Self {
        Self {
            state: SessionLockManagerState::new::<State, _>(dh, |_client| true),
            pending: None,
            owner: None,
            approved_lock: None,
            surfaces: Vec::new(),
            client_unlocked: false,
        }
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.lock_protocol.state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        let pid = confirmation
            .ext_session_lock()
            .client()
            .and_then(|client| client.get_credentials(&self.dh).ok())
            .map(|credentials| credentials.pid);
        // A newer request replaces an older one (dropping the old
        // locker tells its client the lock failed).
        let client = confirmation.ext_session_lock().client().map(|c| c.id());
        let owner = self.lock_protocol.owner.as_ref();
        // Replacement discards abandoned pending surfaces, while a foreign
        // request cannot remove an already approved owner's surfaces.
        self.lock_protocol.surfaces.retain(|(surface, _)| {
            let surface_client = surface.wl_surface().client().map(|c| c.id());
            surface_client
                .as_ref()
                .is_some_and(|id| Some(id) == owner || Some(id) == client.as_ref())
        });
        self.lock_protocol.pending = Some((confirmation, pid));
    }

    fn unlock(&mut self) {
        self.lock_protocol.client_unlocked = true;
        self.lock_protocol.owner = None;
        self.lock_protocol.approved_lock = None;
        self.lock_protocol.surfaces.clear();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(client) = surface.wl_surface().client().map(|c| c.id()) else {
            return;
        };
        let pending_client = self
            .lock_protocol
            .pending
            .as_ref()
            .and_then(|(locker, _)| locker.ext_session_lock().client().map(|c| c.id()));
        // Protocol clients may create surfaces before Locked. Keep those
        // pending surfaces, but expose them only after runtime approval.
        if self.lock_protocol.owner.as_ref() != Some(&client)
            && pending_client.as_ref() != Some(&client)
        {
            return;
        }
        let Some(output) = smithay::output::Output::from_resource(&output) else {
            return;
        };
        let name = output.name();
        let size = self
            .output_entries()
            .into_iter()
            .find(|(entry, ..)| *entry == name)
            .and_then(|(_, out, ..)| out.current_mode().map(|m| (m, out.current_scale())))
            .map(|(mode, scale)| {
                let s = scale.fractional_scale();
                (
                    (f64::from(mode.size.w) / s).round() as u32,
                    (f64::from(mode.size.h) / s).round() as u32,
                )
            })
            .unwrap_or((1280, 800));
        surface.with_pending_state(|state| state.size = Some(size.into()));
        surface.send_configure();
        self.lock_protocol
            .surfaces
            .retain(|(existing_surface, existing)| {
                *existing != name
                    || existing_surface
                        .wl_surface()
                        .client()
                        .map(|c| c.id())
                        .as_ref()
                        != Some(&client)
            });
        self.lock_protocol.surfaces.push((surface, name));
    }
}

// Preserve Smithay's manager/surface handling, but gate release before its
// handler: a protocol error must not invoke the compositor's unlock callback.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [ExtSessionLockManagerV1: SessionLockManagerGlobalData] => SessionLockManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [ExtSessionLockManagerV1: ()] => SessionLockManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(State: [ExtSessionLockSurfaceV1: ExtLockSurfaceUserData] => SessionLockManagerState);

impl Dispatch<ExtSessionLockV1, SessionLockState> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        lock: &ExtSessionLockV1,
        request: Request,
        data: &SessionLockState,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if matches!(request, Request::UnlockAndDestroy)
            && state.lock_protocol.approved_lock.as_ref() != Some(lock)
        {
            lock.post_error(Error::InvalidUnlock, "Lock resource was not approved");
            return;
        }
        <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
            state, client, lock, request, data, dh, data_init,
        );
    }
}

impl State {
    /// A lock request the runtime has not yet decided on, with the
    /// requesting client's pid.
    pub fn take_lock_request(&mut self) -> Option<(SessionLocker, Option<i32>)> {
        self.lock_protocol.pending.take()
    }

    /// Resolve a request after the runtime has checked the supervised PID.
    /// Denial cannot replace the current owner's surfaces or focus targets.
    pub fn resolve_lock_request(&mut self, locker: SessionLocker, approved: bool) {
        if approved {
            self.lock_protocol.approved_lock = Some(locker.ext_session_lock().clone());
            self.lock_protocol.owner = locker.ext_session_lock().client().map(|c| c.id());
            self.lock_protocol.client_unlocked = false;
        }
        let owner = self.lock_protocol.owner.as_ref();
        self.lock_protocol.surfaces.retain(|(surface, _)| {
            surface
                .wl_surface()
                .client()
                .map(|c| c.id())
                .as_ref()
                .is_some_and(|id| Some(id) == owner)
        });
        if approved {
            locker.lock();
        }
    }

    fn approved_lock_surface(&self, surface: &LockSurface) -> bool {
        surface.alive()
            && surface
                .wl_surface()
                .client()
                .map(|c| c.id())
                .as_ref()
                .is_some_and(|id| Some(id) == self.lock_protocol.owner.as_ref())
    }

    /// The lock surface for `output`, if the approved client made one.
    pub fn lock_surface_for(&self, output: &str) -> Option<WlSurface> {
        self.lock_protocol
            .surfaces
            .iter()
            .find(|(surface, name)| name == output && self.approved_lock_surface(surface))
            .map(|(surface, _)| surface.wl_surface().clone())
    }

    /// Every live lock surface.
    pub fn lock_surfaces(&self) -> Vec<WlSurface> {
        self.lock_protocol
            .surfaces
            .iter()
            .filter(|(s, _)| self.approved_lock_surface(s))
            .map(|(s, _)| s.wl_surface().clone())
            .collect()
    }

    /// Whether any lock surface is up.
    pub fn has_lock_surfaces(&self) -> bool {
        self.lock_protocol
            .surfaces
            .iter()
            .any(|(s, _)| self.approved_lock_surface(s))
    }

    /// Whether the lock client asked to unlock since the last check.
    pub fn take_client_unlock(&mut self) -> bool {
        std::mem::take(&mut self.lock_protocol.client_unlocked)
    }
}
