//! GNOME's enabled setting controls the toggle; actual compositor capability
//! controls visibility. Temporary disable and effective temperature are not
//! substitutes for the enabled setting. All bus reads are asynchronous/bounded.
use std::cell::{Cell, RefCell};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gio::prelude::*;

use crate::{services::Tile, Shell};

const NAME: &str = "org.gnome.Mutter.DisplayConfig";
const PATH: &str = "/org/gnome/Mutter/DisplayConfig";
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";

const ADMISSION_MAX_AGE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PeerIdentity {
    pid: u32,
    uid: u32,
    cookie: u64,
}

/// A query chain may show the tile only for its original live control peer,
/// unchanged capability/owner epoch, and bounded original admission age.
struct Admission {
    peer: PeerIdentity,
    epoch: u64,
    started: Instant,
}
impl Admission {
    fn allows(
        &self,
        supported: bool,
        epoch: u64,
        original_alive: bool,
        current: Option<PeerIdentity>,
        now: Instant,
    ) -> bool {
        supported
            && epoch == self.epoch
            && original_alive
            && current == Some(self.peer)
            && fresh_at(self.started, now)
    }
}

fn fresh_at(started: Instant, now: Instant) -> bool {
    now.checked_duration_since(started)
        .is_some_and(|age| age <= ADMISSION_MAX_AGE)
}

fn same_owner(original: &str, current: Option<&str>) -> bool {
    current == Some(original)
}

fn supported_reply(reply: Option<glib::Variant>) -> bool {
    reply
        .and_then(|v| v.try_child_value(0))
        .and_then(|v| v.as_variant())
        .and_then(|v| v.get::<bool>())
        .unwrap_or(false)
}

struct Peer {
    pid: u32,
    uid: u32,
    cookie: u64,
    descriptor: OwnedFd,
}
impl Peer {
    fn capture(shell: &Shell) -> Option<Self> {
        let socket = shell.control.as_ref()?.peer_socket();
        let mut cred = std::mem::MaybeUninit::<libc::ucred>::uninit();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: the borrowed connected socket and writable output remain live.
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                cred.as_mut_ptr().cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<libc::ucred>()
        {
            return None;
        }
        // SAFETY: successful getsockopt initialized the complete credential.
        let cred = unsafe { cred.assume_init() };
        if cred.pid <= 0 || cred.uid != unsafe { libc::geteuid() } || cred.uid == 0 {
            return None;
        }
        let mut cookie = 0_u64;
        let mut len = std::mem::size_of_val(&cookie) as libc::socklen_t;
        // SAFETY: SO_COOKIE identifies the original connected socket even if
        // its numeric FD is later reused during control reconnection.
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_COOKIE,
                (&mut cookie as *mut u64).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<u64>()
        {
            return None;
        }
        let mut fd: libc::c_int = -1;
        let mut len = std::mem::size_of_val(&fd) as libc::socklen_t;
        // SAFETY: success allocates an owned descriptor pinning this socket's
        // original peer; opening a numeric PID later would permit PID reuse.
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERPIDFD,
                (&mut fd as *mut libc::c_int).cast(),
                &mut len,
            )
        } != 0
            || fd < 0
        {
            return None;
        }
        // SAFETY: the kernel allocated fd above; all paths now own/drop it.
        let descriptor = unsafe { OwnedFd::from_raw_fd(fd) };
        if len as usize != std::mem::size_of::<libc::c_int>() {
            return None;
        }
        let peer = Self {
            pid: cred.pid as u32,
            uid: cred.uid,
            cookie,
            descriptor,
        };
        peer.alive().then_some(peer)
    }
    fn identity(&self) -> PeerIdentity {
        PeerIdentity {
            pid: self.pid,
            uid: self.uid,
            cookie: self.cookie,
        }
    }
    fn alive(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.descriptor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one writable pollfd, live borrowed descriptor, zero timeout.
        unsafe { libc::poll(&mut poll, 1, 0) == 0 }
    }
}

// Keep the endpoint and exact request/reply types explicit at each bounded read.
#[allow(clippy::too_many_arguments)]
fn call(
    conn: &gio::DBusConnection,
    name: &str,
    path: &str,
    iface: &str,
    method: &str,
    args: glib::Variant,
    reply: &str,
    done: impl FnOnce(Option<glib::Variant>) + 'static,
) {
    conn.call(
        Some(name),
        path,
        iface,
        method,
        Some(&args),
        Some(glib::VariantTy::new(reply).expect("fixed D-Bus reply signature")),
        gio::DBusCallFlags::NO_AUTO_START,
        2000,
        gio::Cancellable::NONE,
        move |result| done(result.ok()),
    );
}

pub(super) fn attach(tile: &Tile, settings: Option<gio::Settings>, shell: &Rc<RefCell<Shell>>) {
    tile.present(false);
    let Some(settings) = settings else { return };
    tile.show_state(settings.boolean("night-light-enabled"), None);
    let tile = Rc::new(tile.clone());
    let changed_tile = Rc::downgrade(&tile);
    settings.connect_changed(Some("night-light-enabled"), move |settings, _| {
        if let Some(tile) = changed_tile.upgrade() {
            tile.show_state(settings.boolean("night-light-enabled"), None);
        }
    });
    let changed_settings = settings.clone();
    let changed_tile = Rc::downgrade(&tile);
    tile.on_user_toggle(move |enabled| {
        if changed_settings
            .set_boolean("night-light-enabled", enabled)
            .is_err()
        {
            if let Some(tile) = changed_tile.upgrade() {
                tile.show_state(changed_settings.boolean("night-light-enabled"), None);
            }
        }
    });
    let tile = tile.clone();
    let shell = Rc::downgrade(shell);
    gio::bus_get(
        gio::BusType::Session,
        gio::Cancellable::NONE,
        move |connection| {
            let Ok(conn) = connection else { return };
            let busy = Rc::new(Cell::new(false));
            let epoch = Rc::new(Cell::new(0_u64));
            let verified = Rc::new(Cell::new(Instant::now() - Duration::from_secs(4)));
            let hidden = tile.clone();
            let changed_epoch = epoch.clone();
            let subscription = conn.subscribe_to_signal(
                Some(BUS),
                Some(BUS),
                Some("NameOwnerChanged"),
                Some(BUS_PATH),
                Some(NAME),
                gio::DBusSignalFlags::NONE,
                move |_| {
                    changed_epoch.set(changed_epoch.get().wrapping_add(1));
                    hidden.present(false);
                },
            );
            let hidden = tile.clone();
            let changed_epoch = epoch.clone();
            let capability_subscription = conn.subscribe_to_signal(
                Some(NAME),
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                Some(PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    if signal
                        .parameters
                        .try_child_value(0)
                        .as_ref()
                        .and_then(|v| v.str())
                        == Some(NAME)
                    {
                        changed_epoch.set(changed_epoch.get().wrapping_add(1));
                        hidden.present(false);
                    }
                },
            );
            glib::timeout_add_local(Duration::from_millis(500), move || {
                // Own the subscription; ending this source unregisters it.
                let _subscription = &subscription;
                let _capability_subscription = &capability_subscription;
                let Some(shell) = shell.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !fresh_at(verified.get(), Instant::now()) {
                    tile.present(false);
                }
                if busy.get() {
                    return glib::ControlFlow::Continue;
                }
                let Some(peer) = Peer::capture(&shell.borrow()) else {
                    tile.present(false);
                    return glib::ControlFlow::Continue;
                };
                busy.set(true);
                let admission = Admission {
                    peer: peer.identity(),
                    epoch: epoch.get(),
                    started: Instant::now(),
                };
                let (pid, uid) = (peer.pid, peer.uid);
                let finish: Rc<dyn Fn(bool)> = {
                    let (busy, tile, epoch) = (busy.clone(), tile.clone(), epoch.clone());
                    let verified = verified.clone();
                    let peer = Rc::new(peer);
                    let shell = Rc::downgrade(&shell);
                    Rc::new(move |supported| {
                        let current = shell
                            .upgrade()
                            .and_then(|shell| Peer::capture(&shell.borrow()));
                        // capture() admits only a live current peer; the retained
                        // pidfd independently checks the original peer again.
                        let supported = admission.allows(
                            supported,
                            epoch.get(),
                            peer.alive(),
                            current.as_ref().map(Peer::identity),
                            Instant::now(),
                        );
                        if supported {
                            verified.set(admission.started);
                        }
                        tile.present(supported);
                        busy.set(false);
                    })
                };
                let next_conn = conn.clone();
                query_owner(&conn, move |owner| {
                    let Some(owner) = owner else {
                        finish(false);
                        return;
                    };
                    verify_peer(&next_conn, owner, pid, uid, finish);
                });
                glib::ControlFlow::Continue
            });
        },
    );
}

fn query_owner(conn: &gio::DBusConnection, done: impl FnOnce(Option<String>) + 'static) {
    call(
        conn,
        BUS,
        BUS_PATH,
        BUS,
        "GetNameOwner",
        (NAME,).to_variant(),
        "(s)",
        move |reply| {
            done(
                reply
                    .and_then(|v| v.child_value(0).str().map(str::to_owned))
                    .filter(|name| name.starts_with(':') && name.len() <= 255),
            );
        },
    );
}

fn verify_peer(
    conn: &gio::DBusConnection,
    owner: String,
    pid: u32,
    uid: u32,
    finish: Rc<dyn Fn(bool)>,
) {
    let next = conn.clone();
    call(
        conn,
        BUS,
        BUS_PATH,
        BUS,
        "GetConnectionUnixProcessID",
        (owner.as_str(),).to_variant(),
        "(u)",
        move |reply| {
            if reply.and_then(|v| v.child_value(0).get::<u32>()) != Some(pid) {
                finish(false);
                return;
            }
            let later = next.clone();
            call(
                &next,
                BUS,
                BUS_PATH,
                BUS,
                "GetConnectionUnixUser",
                (owner.as_str(),).to_variant(),
                "(u)",
                move |reply| {
                    if reply.and_then(|v| v.child_value(0).get::<u32>()) != Some(uid) {
                        finish(false);
                        return;
                    }
                    let original = owner.clone();
                    let final_conn = later.clone();
                    call(
                        &later,
                        &owner,
                        PATH,
                        "org.freedesktop.DBus.Properties",
                        "Get",
                        (NAME, "NightLightSupported").to_variant(),
                        "(v)",
                        move |reply| {
                            let supported = supported_reply(reply);
                            query_owner(&final_conn, move |current| {
                                finish(supported && same_owner(&original, current.as_deref()))
                            });
                        },
                    );
                },
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admission() -> Admission {
        Admission {
            peer: PeerIdentity {
                pid: 2345,
                uid: 1000,
                cookie: 42,
            },
            epoch: 7,
            started: Instant::now(),
        }
    }

    #[test]
    fn fresh_original_peer_and_supported_capability_are_admitted() {
        let admission = admission();
        assert!(admission.allows(
            true,
            admission.epoch,
            true,
            Some(admission.peer),
            admission.started + Duration::from_secs(1),
        ));
    }

    #[test]
    fn original_start_bounds_entire_chain_including_exact_boundary() {
        let admission = admission();
        assert!(admission.allows(
            true,
            admission.epoch,
            true,
            Some(admission.peer),
            admission.started + ADMISSION_MAX_AGE,
        ));
        assert!(!admission.allows(
            true,
            admission.epoch,
            true,
            Some(admission.peer),
            admission.started + ADMISSION_MAX_AGE + Duration::from_nanos(1),
        ));
        assert!(!admission.allows(
            true,
            admission.epoch,
            true,
            Some(admission.peer),
            admission.started - Duration::from_nanos(1),
        ));
    }

    #[test]
    fn cached_visibility_does_not_restart_age_at_query_completion() {
        let admission = admission();
        let completion = admission.started + Duration::from_millis(2900);
        assert!(admission.allows(
            true,
            admission.epoch,
            true,
            Some(admission.peer),
            completion,
        ));
        // Production stores admission.started, so even a recently completed
        // chain becomes stale at its original deadline rather than 3s later.
        let stored_verification = admission.started;
        assert!(!fresh_at(
            stored_verification,
            completion + Duration::from_millis(101),
        ));
    }

    #[test]
    fn inflight_owner_or_same_owner_capability_invalidation_refuses_reply() {
        let admission = admission();
        // Both actual signal handlers advance this epoch before hiding the tile.
        assert!(!admission.allows(
            true,
            admission.epoch + 1,
            true,
            Some(admission.peer),
            admission.started,
        ));
    }

    #[test]
    fn dead_original_or_missing_current_control_peer_refuses_reply() {
        let admission = admission();
        assert!(!admission.allows(
            true,
            admission.epoch,
            false,
            Some(admission.peer),
            admission.started,
        ));
        // A dead current pidfd is rejected by capture() and reaches this as None.
        assert!(!admission.allows(true, admission.epoch, true, None, admission.started,));
    }

    #[test]
    fn same_pid_and_uid_on_a_reconnected_socket_cannot_finish_old_chain() {
        let admission = admission();
        let replacement = PeerIdentity {
            cookie: admission.peer.cookie + 1,
            ..admission.peer
        };
        assert!(!admission.allows(
            true,
            admission.epoch,
            true,
            Some(replacement),
            admission.started,
        ));
    }

    #[test]
    fn mismatched_pid_or_uid_refuses_reply() {
        let admission = admission();
        for replacement in [
            PeerIdentity {
                pid: admission.peer.pid + 1,
                ..admission.peer
            },
            PeerIdentity {
                uid: admission.peer.uid + 1,
                ..admission.peer
            },
        ] {
            assert!(!admission.allows(
                true,
                admission.epoch,
                true,
                Some(replacement),
                admission.started,
            ));
        }
    }

    #[test]
    fn false_capability_never_admits_even_with_fresh_original_authority() {
        let admission = admission();
        assert!(!admission.allows(
            false,
            admission.epoch,
            true,
            Some(admission.peer),
            admission.started,
        ));
    }

    #[test]
    fn missing_or_replaced_unique_owner_refuses_capability() {
        assert!(same_owner(":1.123", Some(":1.123")));
        assert!(!same_owner(":1.123", Some(":1.124")));
        assert!(!same_owner(":1.123", None));
    }

    #[test]
    fn only_boolean_true_inside_variant_reply_is_supported() {
        assert!(supported_reply(Some((true.to_variant(),).to_variant())));
        assert!(!supported_reply(Some((false.to_variant(),).to_variant())));
        assert!(!supported_reply(Some((123_u32.to_variant(),).to_variant())));
        assert!(!supported_reply(Some((true,).to_variant())));
        assert!(!supported_reply(Some(().to_variant())));
        assert!(!supported_reply(None));
    }
}
