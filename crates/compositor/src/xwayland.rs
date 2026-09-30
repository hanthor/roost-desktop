//! On-demand XWayland server supervision.
//!
//! The supervisor owns the compatibility-server lifecycle behind a small
//! readiness state machine (`Idle → Pending → Ready | Absent`). Spawning
//! is on demand: nothing is attempted until [`XWaylandSupervisor::request`]
//! records the first X11 need, and a missing server binary settles on
//! [`Readiness::Absent`] — a logged, non-fatal report with the native
//! session untouched.
//!
//! The actual spawn is injected as a closure so the state machine stays
//! testable without an X server: unit tests drive [`XWaylandSupervisor::tick`]
//! with fake spawners. The Smithay `XWayland::spawn` wiring lands with the
//! window-model join and installs its spawner there; until then the runtime
//! polls with no spawner and the supervisor rests in `Pending`.

use crate::supervise::RestartPolicy;

/// Why no compatibility server is available. Non-fatal by contract: the
/// session keeps running native windows either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsentReason {
    /// The `xwayland` cargo feature is off or no spawner is installed,
    /// so starting was never possible in this build.
    Unsupported,
    /// The server binary is missing or exited during startup.
    SpawnFailed,
}

/// What [`XWaylandSupervisor::readiness`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Never requested: no X11 need has been recorded.
    Idle,
    /// Requested and awaiting a spawn attempt (or its backoff delay).
    Pending,
    /// The server is up on the given display number.
    Ready {
        /// X11 display number, for the log line and future `DISPLAY`.
        display: u32,
    },
    /// Settled without a server. The report is logged exactly once.
    Absent {
        /// Why the server is unavailable.
        reason: AbsentReason,
    },
}

/// On-demand supervisor for the XWayland server.
///
/// Matches the [`crate::supervise`] shape: bounded attempts
/// (`1 + max_attempts` spawns, then settle), capped exponential backoff
/// between retries, and exactly one log line per terminal transition.
#[derive(Debug)]
pub struct XWaylandSupervisor {
    policy: RestartPolicy,
    requested: bool,
    readiness: Readiness,
    /// Spawn attempts so far (initial spawn plus restarts).
    attempts: u32,
    /// Earliest `now_ms` for the next attempt (backoff gate).
    next_allowed_ms: u64,
    start_logged: bool,
    absent_logged: bool,
}

impl XWaylandSupervisor {
    /// Build an idle supervisor: three restarts after the initial spawn,
    /// 500 ms base backoff capped at 5 s.
    pub fn new() -> Self {
        Self {
            policy: RestartPolicy::new(3, 500, 5_000),
            requested: false,
            readiness: Readiness::Idle,
            attempts: 0,
            next_allowed_ms: 0,
            start_logged: false,
            absent_logged: false,
        }
    }

    /// Record the first X11 need. Idempotent: later calls change nothing.
    /// Moves `Idle` to `Pending`; every other state is left alone.
    pub fn request(&mut self) {
        self.requested = true;
        if self.readiness == Readiness::Idle {
            self.readiness = Readiness::Pending;
        }
    }

    /// Current readiness. Never blocks and never spawns.
    pub fn readiness(&self) -> Readiness {
        self.readiness
    }

    /// Advance the state machine. Attempts a spawn when requested, the
    /// state is still pending, the backoff gate has passed, and a spawner
    /// is installed. With no spawner the call is a no-op and the
    /// supervisor stays pending — the runtime polls this way until the
    /// window-model join installs the real Smithay spawn.
    pub fn tick(
        &mut self,
        now_ms: u64,
        spawn: Option<&mut dyn FnMut() -> Result<u32, AbsentReason>>,
    ) {
        if !self.requested {
            return;
        }
        match self.readiness {
            Readiness::Ready { .. } | Readiness::Absent { .. } => return,
            Readiness::Idle => {
                self.readiness = Readiness::Pending;
                return;
            }
            Readiness::Pending => {}
        }
        if now_ms < self.next_allowed_ms {
            return;
        }
        let spawn = match spawn {
            Some(spawn) => spawn,
            None => return,
        };
        self.attempts += 1;
        match spawn() {
            Ok(display) => {
                self.readiness = Readiness::Ready { display };
                if !self.start_logged {
                    self.start_logged = true;
                    eprintln!("roost-compositor: xwayland: server started on :{display}");
                }
            }
            Err(reason) => {
                if self.attempts > self.policy.max_attempts {
                    self.readiness = Readiness::Absent { reason };
                    if !self.absent_logged {
                        self.absent_logged = true;
                        eprintln!(
                            "roost-compositor: xwayland: unavailable ({reason:?}); native session continues"
                        );
                    }
                } else {
                    self.next_allowed_ms =
                        now_ms.saturating_add(self.policy.next_delay(self.attempts - 1));
                }
            }
        }
    }
}

impl Default for XWaylandSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

/// Fallback title when an X11 window carries no title.
pub const UNTITLED_X11_TITLE: &str = "Untitled window";

/// Translate X11 window properties into the snapshot fields the shell
/// already consumes: title from the X11 title, app id from WM_CLASS
/// (class preferred, instance as tiebreak). Pure function with no
/// compositor access, so it reads like the existing snapshot-mapping
/// helpers. Empty properties resolve to the fixed fallbacks, never to
/// blanks. (xwayland feature only.)
#[cfg(feature = "xwayland")]
pub fn map_x11_identity(title: &str, class: &str, instance: &str) -> (String, Option<String>) {
    let title = if title.is_empty() {
        UNTITLED_X11_TITLE.to_owned()
    } else {
        title.to_owned()
    };
    let app_id = if !class.is_empty() {
        Some(class.to_owned())
    } else if !instance.is_empty() {
        Some(instance.to_owned())
    } else {
        None
    };
    (title, app_id)
}

/// X11 window-manager happenings the [`crate::State`] handlers queue
/// for the window manager's `reconcile` drain, so all scene mutation
/// stays on the one manager call path. (xwayland feature only.)
#[cfg(feature = "xwayland")]
#[derive(Debug, Clone)]
pub enum X11ManagerEvent {
    /// An X11 window asked to be mapped (already granted via
    /// `set_mapped(true)` in the handler). Boxed: the surface is
    /// ~270 bytes and dwarfs the other variants.
    MapRequest(Box<smithay::xwayland::X11Surface>),
    /// An X11 window was unmapped (X11 window id).
    Unmapped(u32),
    /// An X11 window was destroyed (X11 window id).
    Destroyed(u32),
    /// An X11 window asked for a new size; the manager applies what
    /// fits its geometry and advertises it back.
    ConfigureRequest {
        /// X11 window id.
        id: u32,
        /// Requested size, if the client named one.
        size: Option<(u32, u32)>,
    },
    /// An X11 window property changed; the manager re-reads identity.
    Property(u32),
    /// An X11 maximize/unmaximize request (X11 window id, maximize?).
    Maximize(u32, bool),
    /// An X11 fullscreen/unfullscreen request (X11 window id, on?).
    Fullscreen(u32, bool),
}

/// Spawn the compatibility server and hook its event source into the
/// loop: on `Ready` the X11 window manager starts against the
/// privileged socket; on `Error` the failure is logged (the windows
/// simply never arrive). Returns the display number on a successful
/// spawn so the supervisor can report its single start line; a
/// missing binary (or any spawn failure) resolves to
/// [`AbsentReason::SpawnFailed`]. (xwayland feature only.)
#[cfg(feature = "xwayland")]
pub fn spawn_xwayland(
    dh: &smithay::reexports::wayland_server::DisplayHandle,
    loop_handle: &calloop::LoopHandle<'static, crate::runtime::Runtime>,
    pending_client: &mut Option<smithay::reexports::wayland_server::Client>,
) -> Result<u32, AbsentReason> {
    use smithay::xwayland::XWayland;

    // `ROOST_XWAYLAND_DEBUG=1` inherits the server's stdio for
    // troubleshooting (focus routing, startup failures); default
    // stays silent.
    let debug = std::env::var("ROOST_XWAYLAND_DEBUG").is_ok_and(|value| value == "1");
    let stdio = || {
        if debug {
            std::process::Stdio::inherit()
        } else {
            std::process::Stdio::null()
        }
    };
    let envs: [(String, String); 0] = [];
    let (xwayland, client) = XWayland::spawn(
        dh,
        None,
        envs,
        true,
        stdio(),
        stdio(),
        // The server client carries Smithay's `XWaylandClientData`
        // (not our `ClientState`); `client_compositor_state` serves
        // both, so nothing is installed here.
        |_| {},
    )
    .map_err(|_| AbsentReason::SpawnFailed)?;
    let display = xwayland.display_number();
    *pending_client = Some(client);
    if loop_handle
        .insert_source(
            xwayland,
            |event, _, runtime: &mut crate::runtime::Runtime| {
                runtime.on_xwayland_event(event);
            },
        )
        .is_err()
    {
        *pending_client = None;
        return Err(AbsentReason::SpawnFailed);
    }
    Ok(display)
}

/// Handle one XWayland server event: start the X11 window manager on
/// `Ready`, log a non-fatal report on `Error`. Called from the
/// event-source callback with the runtime in scope.
/// (xwayland feature only.)
#[cfg(feature = "xwayland")]
pub fn on_xwayland_event(
    runtime: &mut crate::runtime::Runtime,
    event: smithay::xwayland::XWaylandEvent,
) {
    use smithay::xwayland::{X11Wm, XWaylandEvent};

    match event {
        XWaylandEvent::Ready {
            x11_socket,
            display_number,
        } => {
            let Some(client) = runtime.take_pending_x11_client() else {
                eprintln!("roost-compositor: xwayland: ready with no pending client; ignoring");
                return;
            };
            match X11Wm::start_wm(runtime.loop_handle(), x11_socket, client) {
                Ok(xwm) => {
                    runtime.state_mut().xwm = Some(xwm);
                    eprintln!(
                        "roost-compositor: xwayland: window manager started on :{display_number}"
                    );
                }
                Err(error) => {
                    eprintln!("roost-compositor: xwayland: window manager failed: {error}");
                }
            }
        }
        XWaylandEvent::Error => {
            runtime.take_pending_x11_client();
            eprintln!(
                "roost-compositor: xwayland: server exited during startup; X11 windows unavailable"
            );
        }
    }
}

#[cfg(feature = "xwayland")]
mod handlers {
    use smithay::wayland::xwayland_shell::XWaylandShellHandler;
    use smithay::xwayland::xwm::{Reorder, X11Surface, X11Window, XwmHandler, XwmId};
    use smithay::xwayland::X11Wm;

    use super::X11ManagerEvent;
    use crate::runtime::Runtime;
    use crate::State;

    /// Queue one manager event on the protocol state.
    fn push(state: &mut State, event: X11ManagerEvent) {
        state.x11_events.push(event);
    }

    /// Shared map-request handling: grant, then queue. Placement and
    /// scene work stay in the manager's reconcile drain.
    fn on_map_request(state: &mut State, window: X11Surface) {
        if window.is_override_redirect() {
            return;
        }
        if window.set_mapped(true).is_err() {
            return;
        }
        push(state, X11ManagerEvent::MapRequest(Box::new(window)));
    }

    /// Shared configure-request handling: placement stays
    /// manager-owned (x/y ignored); the requested size flows to the
    /// manager, which advertises its geometry back via configure.
    fn on_configure_request(state: &mut State, window: X11Surface, w: Option<u32>, h: Option<u32>) {
        let size = match (w, h) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        };
        push(
            state,
            X11ManagerEvent::ConfigureRequest {
                id: window.window_id(),
                size,
            },
        );
    }

    /// Association takes effect on commit; the manager reads the
    /// live `wl_surface()` every reconcile and re-asserts seat focus
    /// then, so no event is needed here.
    fn on_surface_associated() {}

    /// The running X11 window manager, or a panic naming the broken
    /// invariant (callbacks only fire while it runs).
    fn xwm_slot(state: &mut State) -> &mut X11Wm {
        state
            .xwm
            .as_mut()
            .expect("xwm callback without a running X11Wm")
    }

    fn state_of(state: &mut State) -> &mut State {
        state
    }

    fn state_of_runtime(runtime: &mut Runtime) -> &mut State {
        runtime.state_mut()
    }

    macro_rules! impl_shell_handler {
        ($target:ty, $state:expr) => {
            impl XWaylandShellHandler for $target {
                fn xwayland_shell_state(
                    &mut self,
                ) -> &mut smithay::wayland::xwayland_shell::XWaylandShellState
                {
                    &mut $state(self).xwayland_shell_state
                }

                fn surface_associated(
                    &mut self,
                    _xwm: XwmId,
                    _wl_surface: smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
                    _surface: X11Surface,
                ) {
                    on_surface_associated();
                }
            }
        };
    }

    macro_rules! impl_xwm_handler {
        ($target:ty, $state:expr) => {
            impl XwmHandler for $target {
                fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
                    xwm_slot($state(self))
                }

                fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {
                    // Unmapped yet: nothing to manage until map_window_request.
                }

                fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {
                    // Never managed (no intercept on override-redirect).
                }

                fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    on_map_request($state(self), window);
                }

                fn mapped_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

                fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
                    push($state(self), X11ManagerEvent::Unmapped(window.window_id()));
                }

                fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
                    push($state(self), X11ManagerEvent::Destroyed(window.window_id()));
                }

                fn configure_request(
                    &mut self,
                    _xwm: XwmId,
                    window: X11Surface,
                    _x: Option<i32>,
                    _y: Option<i32>,
                    w: Option<u32>,
                    h: Option<u32>,
                    _reorder: Option<Reorder>,
                ) {
                    on_configure_request($state(self), window, w, h);
                }

                fn configure_notify(
                    &mut self,
                    _xwm: XwmId,
                    _window: X11Surface,
                    _geometry: smithay::utils::Rectangle<i32, smithay::utils::Logical>,
                    _above: Option<X11Window>,
                ) {
                }

                fn resize_request(
                    &mut self,
                    _xwm: XwmId,
                    _window: X11Surface,
                    _button: u32,
                    _resize_edge: smithay::xwayland::xwm::ResizeEdge,
                ) {
                    // No interactive drag-to-resize in this slice; ignored.
                }

                fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {
                    // No interactive drag-to-move in this slice; ignored.
                }

                fn property_notify(
                    &mut self,
                    _xwm: XwmId,
                    window: X11Surface,
                    _property: smithay::xwayland::xwm::WmWindowProperty,
                ) {
                    // Title/class may have changed; the manager
                    // re-reads identity on the next reconcile.
                    push($state(self), X11ManagerEvent::Property(window.window_id()));
                }

                fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Maximize(window.window_id(), true),
                    );
                }

                fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Maximize(window.window_id(), false),
                    );
                }

                fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Fullscreen(window.window_id(), true),
                    );
                }

                fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
                    push(
                        $state(self),
                        X11ManagerEvent::Fullscreen(window.window_id(), false),
                    );
                }
            }
        };
    }

    // Protocol dispatch runs against `State` (the display data), while
    // `X11Wm::start_wm` binds the loop-data `Runtime`: both implement
    // the two handler traits over the same queue.
    impl_shell_handler!(State, state_of);
    impl_shell_handler!(Runtime, state_of_runtime);
    impl_xwm_handler!(State, state_of);
    impl_xwm_handler!(Runtime, state_of_runtime);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn fresh_supervisor_is_idle_and_tick_without_request_never_spawns() {
        let mut sup = XWaylandSupervisor::new();
        assert_eq!(sup.readiness(), Readiness::Idle);
        let calls = Cell::new(0);
        let mut spawn = || {
            calls.set(calls.get() + 1);
            Ok(7)
        };
        sup.tick(0, Some(&mut spawn));
        sup.tick(1_000_000, Some(&mut spawn));
        assert_eq!(calls.get(), 0);
        assert_eq!(sup.readiness(), Readiness::Idle);
    }

    #[test]
    fn request_moves_idle_to_pending_and_is_idempotent() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        assert_eq!(sup.readiness(), Readiness::Pending);
        sup.request();
        assert_eq!(sup.readiness(), Readiness::Pending);
    }

    #[test]
    fn successful_spawn_on_first_attempt_yields_ready() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut spawn = || {
            calls.set(calls.get() + 1);
            Ok(7)
        };
        sup.tick(0, Some(&mut spawn));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Ready { display: 7 });
    }

    #[test]
    fn spawn_failure_retries_with_backoff_then_settles_absent_after_budget() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        // Attempt 1 at t=0; backoff gate is 500 ms (base delay).
        sup.tick(0, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Attempt 2 at t=500; next gate is 1_000 ms.
        sup.tick(500, Some(&mut failing));
        assert_eq!(calls.get(), 2);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Attempt 3 at t=1500; next gate is 2_000 ms.
        sup.tick(1500, Some(&mut failing));
        assert_eq!(calls.get(), 3);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Attempt 4 at t=3500 exhausts the 3-restart budget: Absent.
        // Missing binary is non-fatal: no panic, session runs on native.
        sup.tick(3500, Some(&mut failing));
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }

    #[test]
    fn backoff_gate_blocks_tick_before_delay_passes() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        sup.tick(0, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        // 499 ms is still inside the 500 ms first backoff: no attempt.
        sup.tick(499, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Pending);
    }

    #[test]
    fn terminal_states_are_sticky() {
        // Ready never changes on further ticks.
        let mut ready = XWaylandSupervisor::new();
        ready.request();
        let ok_calls = Cell::new(0);
        let mut ok = || -> Result<u32, AbsentReason> {
            ok_calls.set(ok_calls.get() + 1);
            Ok(7)
        };
        ready.tick(0, Some(&mut ok));
        assert_eq!(ready.readiness(), Readiness::Ready { display: 7 });
        let fail_calls = Cell::new(0);
        let mut fail = || -> Result<u32, AbsentReason> {
            fail_calls.set(fail_calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        ready.tick(u64::MAX, Some(&mut fail));
        ready.tick(u64::MAX, None);
        assert_eq!(ok_calls.get(), 1);
        assert_eq!(fail_calls.get(), 0);
        assert_eq!(ready.readiness(), Readiness::Ready { display: 7 });

        // Absent never changes on further ticks.
        let mut absent = XWaylandSupervisor::new();
        absent.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        absent.tick(0, Some(&mut failing));
        absent.tick(500, Some(&mut failing));
        absent.tick(1500, Some(&mut failing));
        absent.tick(3500, Some(&mut failing));
        assert_eq!(
            absent.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
        absent.tick(u64::MAX, Some(&mut failing));
        absent.tick(u64::MAX, None);
        assert_eq!(calls.get(), 4);
        assert_eq!(
            absent.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }

    #[test]
    fn no_spawner_stays_pending_with_zero_attempts() {
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        // Pre-join wiring: the runtime polls with no spawner installed.
        sup.tick(0, None);
        sup.tick(u64::MAX, None);
        assert_eq!(sup.readiness(), Readiness::Pending);
        // Nothing was attempted: the first real spawner still runs attempt 1.
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        sup.tick(u64::MAX, Some(&mut failing));
        assert_eq!(calls.get(), 1);
        assert_eq!(sup.readiness(), Readiness::Pending);
    }

    #[test]
    fn default_equals_new() {
        let a = XWaylandSupervisor::default();
        let b = XWaylandSupervisor::new();
        assert_eq!(a.readiness(), b.readiness());
        assert_eq!(a.readiness(), Readiness::Idle);
    }

    #[test]
    fn untitled_fallback_const_is_stable_and_nonblank() {
        // Runs with and without the `xwayland` feature (unlike
        // `identity_tests`): pins the mapping fallback the snapshot
        // path depends on in every suite configuration.
        assert!(!UNTITLED_X11_TITLE.is_empty());
        assert_eq!(UNTITLED_X11_TITLE, "Untitled window");
    }

    #[test]
    fn unsupported_spawner_settles_absent_unsupported_after_budget() {
        // The feature-off report path: `Unsupported` must propagate to
        // the terminal state exactly like `SpawnFailed`, never succeed.
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut unsupported = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::Unsupported)
        };
        sup.tick(0, Some(&mut unsupported));
        sup.tick(500, Some(&mut unsupported));
        sup.tick(1500, Some(&mut unsupported));
        assert_eq!(sup.readiness(), Readiness::Pending);
        sup.tick(3500, Some(&mut unsupported));
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::Unsupported
            }
        );
    }

    #[test]
    fn request_on_terminal_absent_never_revives() {
        // Late X11 needs after the settle must not reopen the server
        // attempt: the report stays logged-once and terminal.
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut failing = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            Err(AbsentReason::SpawnFailed)
        };
        sup.tick(0, Some(&mut failing));
        sup.tick(500, Some(&mut failing));
        sup.tick(1500, Some(&mut failing));
        sup.tick(3500, Some(&mut failing));
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
        sup.request();
        sup.tick(u64::MAX, Some(&mut failing));
        sup.tick(u64::MAX, None);
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }

    #[test]
    fn production_style_spawn_with_empty_path_fails_cleanly_to_spawn_failed() {
        // Absent-binary report path: the production-style spawn lookup
        // for the `Xwayland` binary with an empty PATH must fail
        // cleanly (no panic, no process) and resolve to `SpawnFailed`,
        // driving the supervisor to the non-fatal `Absent` settle with
        // the native session untouched. No real loop is involved.
        let mut sup = XWaylandSupervisor::new();
        sup.request();
        let calls = Cell::new(0);
        let mut production_style = || -> Result<u32, AbsentReason> {
            calls.set(calls.get() + 1);
            match std::process::Command::new("Xwayland")
                .env_clear()
                .env("PATH", "")
                .arg("-help")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("Xwayland resolved with an empty PATH; expected a clean lookup failure");
                }
                Err(_) => Err(AbsentReason::SpawnFailed),
            }
        };
        sup.tick(0, Some(&mut production_style));
        sup.tick(500, Some(&mut production_style));
        sup.tick(1500, Some(&mut production_style));
        assert_eq!(sup.readiness(), Readiness::Pending);
        sup.tick(3500, Some(&mut production_style));
        assert_eq!(calls.get(), 4);
        assert_eq!(
            sup.readiness(),
            Readiness::Absent {
                reason: AbsentReason::SpawnFailed
            }
        );
    }
}

#[cfg(all(test, feature = "xwayland"))]
mod identity_tests {
    use super::{map_x11_identity, UNTITLED_X11_TITLE};

    #[test]
    fn full_title_and_class() {
        let (title, app_id) = map_x11_identity("Terminal", "xterm", "xterm-instance");
        assert_eq!(title, "Terminal");
        assert_eq!(app_id.as_deref(), Some("xterm"));
    }

    #[test]
    fn empty_title_falls_back_to_untitled_const() {
        let (title, app_id) = map_x11_identity("", "xterm", "xterm-instance");
        assert_eq!(title, UNTITLED_X11_TITLE);
        assert_eq!(app_id.as_deref(), Some("xterm"));
    }

    #[test]
    fn class_preferred_over_instance() {
        let (_, app_id) = map_x11_identity("Editor", "emacs", "emacs-instance");
        assert_eq!(app_id.as_deref(), Some("emacs"));
    }

    #[test]
    fn instance_used_when_class_empty() {
        let (_, app_id) = map_x11_identity("Editor", "", "emacs-instance");
        assert_eq!(app_id.as_deref(), Some("emacs-instance"));
    }

    #[test]
    fn both_class_and_instance_empty_yields_no_app_id() {
        let (title, app_id) = map_x11_identity("Editor", "", "");
        assert_eq!(title, "Editor");
        assert_eq!(app_id, None);
    }

    #[test]
    fn title_preserved_verbatim_without_trimming() {
        let (title, _) = map_x11_identity("  padded title  ", "xterm", "");
        assert_eq!(title, "  padded title  ");
    }

    #[test]
    fn all_fields_empty_yields_untitled_and_no_app_id() {
        // The fully-bare window: every missing-field fallback at once.
        // The snapshot entry must never carry a blank title, and with
        // no WM_CLASS at all there is no app id to report.
        let (title, app_id) = map_x11_identity("", "", "");
        assert_eq!(title, UNTITLED_X11_TITLE);
        assert!(!title.is_empty());
        assert_eq!(app_id, None);
    }
}
