//! Frame timing protocols (#89, P-SY-06): presentation-time, fifo and
//! commit-timing, the three GNOME 51's Mutter offers so video players,
//! games and Vulkan's FIFO present mode can pace themselves.
//!
//! - **presentation-time v2** (CLOCK_MONOTONIC, as Mutter): feedback is
//!   taken from the surfaces a frame actually drew and marked presented
//!   when that frame reaches the screen. On hardware that is the page
//!   flip, with the kernel's vblank timestamp and sequence and Mutter's
//!   KMS flags (vsync, hardware clock, hardware completion); nested, it
//!   is the host swap, timed by the compositor's clock. Surfaces not
//!   drawn keep their feedback until they are (smithay discards it when
//!   a later commit supersedes it), as the protocol asks.
//! - **fifo v1**: a barrier set by a content update is released on the
//!   next refresh cycle. Every surface that receives frame callbacks is
//!   released, drawn or not, so a client on a hidden workspace keeps
//!   making progress instead of stalling.
//! - **commit-timing v1**: a timed update is applied on the refresh
//!   cycle whose next presentation reaches its timestamp.
//!
//! Smithay 0.7 places the blockers (managed mode); this module releases
//! them and lets the blocked transactions apply.

use std::time::Duration;

use smithay::{
    delegate_commit_timing, delegate_fifo, delegate_presentation,
    desktop::utils::{take_presentation_feedback_surface_tree, OutputPresentationFeedback},
    output::Output,
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{protocol::wl_surface::WlSurface, Client, Resource},
    },
    utils::{Clock, Monotonic, Time},
    wayland::{
        commit_timing::{CommitTimerBarrierStateUserData, CommitTimingManagerState},
        compositor::{with_surface_tree_downward, CompositorHandler, TraversalAction},
        fifo::{FifoBarrierCachedState, FifoManagerState},
        presentation::{PresentationState, Refresh},
        seat::WaylandFocus,
    },
};

use crate::windows::WindowManager;
use crate::State;

/// Refresh interval assumed when an output reports no mode (60 Hz).
pub const DEFAULT_REFRESH: Duration = Duration::from_nanos(16_666_667);

/// The frame timing globals and the clock they report in.
pub(crate) struct FrameTiming {
    _presentation: PresentationState,
    _fifo: FifoManagerState,
    _commit_timing: CommitTimingManagerState,
    clock: Clock<Monotonic>,
}

impl FrameTiming {
    pub(crate) fn new(dh: &smithay::reexports::wayland_server::DisplayHandle) -> Self {
        Self {
            _presentation: PresentationState::new::<State>(dh, libc::CLOCK_MONOTONIC as u32),
            _fifo: FifoManagerState::new::<State>(dh),
            _commit_timing: CommitTimingManagerState::new::<State>(dh),
            clock: Clock::new(),
        }
    }
}

/// An output's refresh interval from its current mode.
pub fn refresh_of(output: &Output) -> Duration {
    output
        .current_mode()
        .map(|mode| mode.refresh)
        .filter(|mhz| *mhz > 0)
        .map(|mhz| Duration::from_nanos(1_000_000_000_000 / mhz as u64))
        .unwrap_or(DEFAULT_REFRESH)
}

/// Take the pending presentation feedback of every surface tree in
/// `roots`: the frame about to be shown on `output` drew them.
pub fn take_feedback(roots: &[WlSurface], output: &Output) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    for root in roots {
        take_presentation_feedback_surface_tree(
            root,
            &mut feedback,
            |_, _| Some(output.clone()),
            |_, _| wp_presentation_feedback::Kind::empty(),
        );
    }
    feedback
}

/// An identical image can still owe a client a real refresh/presentation.
/// Include callbacks, presentation, FIFO barriers and deferred commit timers.
#[doc(hidden)]
pub fn pending_frame_work(roots: &[WlSurface]) -> bool {
    pending_work(roots, true)
}

/// Refresh obligations apply to all mapped surfaces, but only presentation
/// requests belonging to this output's drawn surfaces can require scanout.
/// A hidden request stays pending until that surface is actually drawn.
#[doc(hidden)]
pub fn pending_output_work(roots: &[WlSurface], drawn: &[WlSurface]) -> bool {
    pending_work(roots, false) || pending_work(drawn, true)
}

fn pending_work(roots: &[WlSurface], include_presentation: bool) -> bool {
    use smithay::wayland::compositor::SurfaceAttributes;
    use smithay::wayland::presentation::PresentationFeedbackCachedState;
    let mut pending = false;
    for root in roots {
        with_surface_tree_downward(
            root,
            (),
            |_, _, _| TraversalAction::DoChildren(()),
            |_, states, _| {
                pending |= !states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .frame_callbacks
                    .is_empty()
                    || (include_presentation
                        && !states
                            .cached_state
                            .get::<PresentationFeedbackCachedState>()
                            .current()
                            .callbacks
                            .is_empty())
                    || states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .is_some()
                    || states
                        .data_map
                        .get::<CommitTimerBarrierStateUserData>()
                        .is_some_and(|timers| timers.lock().unwrap().next_deadline().is_some());
            },
            |_, _, _| true,
        );
    }
    pending
}

/// Every surface tree that receives frame callbacks: mapped windows on
/// any workspace, layer surfaces, popups, the lock screen, a drag icon
/// and X11 windows. Fifo barriers and commit timers advance on all of
/// them each refresh cycle.
pub fn frame_roots(state: &State, manager: &WindowManager) -> Vec<WlSurface> {
    let mut roots: Vec<WlSurface> = state
        .toplevels()
        .iter()
        .map(|t| t.wl_surface().clone())
        .collect();
    let layers: Vec<WlSurface> = crate::layer::layer_layout(state)
        .into_iter()
        .map(|(s, _, _)| s)
        .collect();
    let parents: Vec<WlSurface> = roots.iter().chain(layers.iter()).cloned().collect();
    roots.extend(layers);
    for parent in parents {
        for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(&parent) {
            roots.push(popup.wl_surface().clone());
        }
    }
    roots.extend(state.dnd_icon());
    roots.extend(state.lock_surfaces());
    #[cfg(feature = "xwayland")]
    roots.extend(manager.x11_surfaces());
    #[cfg(not(feature = "xwayland"))]
    let _ = manager;
    roots
}

/// Surface trees a frame draws, each with where it sits in the global
/// logical space (to find its output). The lock screen is per output
/// and not listed: callers add the lock surface they drew. While
/// locked nothing else is drawn.
pub fn drawn_roots(
    state: &State,
    manager: &WindowManager,
    locked: bool,
) -> Vec<(
    WlSurface,
    smithay::utils::Point<i32, smithay::utils::Logical>,
)> {
    if locked {
        return Vec::new();
    }
    let mut roots = Vec::new();
    let mut parents = Vec::new();
    for (window, geometry) in manager.visible_windows() {
        if let Some(surface) = window.wl_surface() {
            let center = smithay::utils::Point::from((
                geometry.loc.x + geometry.size.w / 2,
                geometry.loc.y + geometry.size.h / 2,
            ));
            parents.push((surface.clone().into_owned(), center));
            roots.push((surface.into_owned(), center));
        }
    }
    for (surface, (x, y), _) in crate::layer::layer_layout(state) {
        parents.push((surface.clone(), (x, y).into()));
        roots.push((surface, (x, y).into()));
    }
    for (parent, at) in parents {
        for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(&parent) {
            roots.push((popup.wl_surface().clone(), at));
        }
    }
    if let Some(icon) = state.dnd_icon() {
        roots.push((icon, manager.pointer_pos().to_i32_round()));
    }
    roots
}

impl State {
    /// Now on the presentation clock (CLOCK_MONOTONIC).
    pub fn presentation_now(&self) -> Time<Monotonic> {
        self.frame_timing.clock.now()
    }

    /// One refresh cycle happened at `time`: release every fifo
    /// barrier in `roots` and every commit timer due by the next
    /// presentation (`time + refresh`), then apply the content updates
    /// they held back.
    pub fn refresh_cycle(&mut self, roots: &[WlSurface], time: Time<Monotonic>, refresh: Duration) {
        let deadline: Time<Monotonic> = Time::from(Duration::from(time) + refresh);
        let mut released: Vec<Client> = Vec::new();
        for root in roots {
            with_surface_tree_downward(
                root,
                (),
                |_, _, _| TraversalAction::DoChildren(()),
                |surface, states, _| {
                    let mut signaled = false;
                    let barrier = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take();
                    if let Some(barrier) = barrier {
                        barrier.signal();
                        signaled = true;
                    }
                    if let Some(timers) = states.data_map.get::<CommitTimerBarrierStateUserData>() {
                        signaled |= timers.lock().unwrap().signal_until(deadline);
                    }
                    if signaled {
                        if let Some(client) = surface.client() {
                            if !released.iter().any(|c| c.id() == client.id()) {
                                released.push(client);
                            }
                        }
                    }
                },
                |_, _, _| true,
            );
        }
        let dh = self.dh.clone();
        for client in released {
            self.client_compositor_state(&client)
                .blocker_cleared(self, &dh);
        }
    }
}

/// The nested backend's end of a frame: everything drawn is presented
/// now on the single output, then the refresh cycle advances. `seq`
/// counts frames (the host gives no vblank sequence). Tests drive the
/// same call.
pub fn present_nested_frame(state: &mut State, manager: &WindowManager, locked: bool, seq: u64) {
    let Some(output) = state.primary_output() else {
        return;
    };
    let mut drawn: Vec<WlSurface> = drawn_roots(state, manager, locked)
        .into_iter()
        .map(|(surface, _)| surface)
        .collect();
    if locked {
        drawn.extend(
            state
                .primary_output_name()
                .and_then(|name| state.lock_surface_for(&name)),
        );
    }
    let mut feedback = take_feedback(&drawn, &output);
    let now = state.presentation_now();
    let refresh = refresh_of(&output);
    feedback.presented::<_, Monotonic>(
        now,
        Refresh::fixed(refresh),
        seq,
        wp_presentation_feedback::Kind::empty(),
    );
    let roots = frame_roots(state, manager);
    state.refresh_cycle(&roots, now, refresh);
}

delegate_presentation!(State);
delegate_fifo!(State);
delegate_commit_timing!(State);
