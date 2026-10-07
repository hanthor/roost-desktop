//! Bounded ownership/epoch admission for native monitor refresh.
//!
//! This is coordination, not connector acquisition or hotplug reconciliation.
//! An expired worker still owns its single slot until it actually returns.
use std::time::{Duration, Instant};

pub const REFRESH_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEvent {
    Changed,
    Removed,
    Added,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    pub device: u64,
    pub epoch: u64,
    pub started: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Accepted,
    RetryPending,
    Unavailable,
    Foreign,
}

/// One original notifier-fault episode. Repeated failures cannot renew the
/// native refresh retry budget. Only a genuinely matched completed commit
/// demonstrates that the same original notifier recovered.
#[derive(Debug, Default)]
pub struct EventFault {
    latched: bool,
}
impl EventFault {
    pub fn revoke_once(&mut self) -> bool {
        if self.latched {
            return false;
        }
        self.latched = true;
        true
    }
    pub fn matched_completion(&mut self) {
        self.latched = false;
    }
}

#[derive(Debug)]
pub struct RefreshState {
    device: u64,
    epoch: u64,
    active: bool,
    original_present: bool,
    pending: bool,
    attempts: u8,
    validated_epoch: Option<u64>,
    in_flight: Option<Ticket>,
}

impl RefreshState {
    pub fn new(device: u64) -> Self {
        Self {
            device,
            epoch: 1,
            active: true,
            original_present: true,
            pending: true,
            attempts: 0,
            validated_epoch: None,
            in_flight: None,
        }
    }

    /// Bookkeeping may retain original globals while the VT is paused; this
    /// alone never authorizes capture, brightness, KMS work or acceptance.
    pub fn original_device_present(&self) -> bool {
        self.original_present
    }

    pub fn original_available(&self) -> bool {
        self.active && self.original_present
    }

    /// Current original-device lifecycle stamp for commit correlation only.
    /// It is NOT discovery/brightness authority while refresh remains pending.
    pub fn commit_epoch(&self) -> Option<u64> {
        self.original_available().then_some(self.epoch)
    }

    /// Compositor-owned discovery generation. None means revoked/pending or
    /// failed refresh, even if the original char FD remains physically open.
    pub fn ownership_generation(&self) -> Option<u64> {
        self.validated_epoch
            .filter(|epoch| self.original_available() && *epoch == self.epoch)
    }

    fn invalidate(&mut self) {
        self.validated_epoch = None;
        match self.epoch.checked_add(1) {
            Some(epoch) => self.epoch = epoch,
            None => self.original_present = false,
        }
        self.pending = self.original_present;
        self.attempts = 0;
    }

    /// True means optional discovery metadata must be invalidated immediately.
    /// A new device with a reused dev_t never revives this original ownership.
    pub fn device_event(&mut self, device: u64, event: DeviceEvent) -> bool {
        if device != self.device {
            return false;
        }
        if event == DeviceEvent::Removed {
            self.original_present = false;
        }
        self.invalidate();
        true
    }

    /// Pause invalidates in-flight observations; activation schedules discovery.
    pub fn set_active(&mut self, active: bool) -> bool {
        if self.active == active {
            return false;
        }
        self.active = active;
        self.invalidate();
        true
    }

    /// Resume can require reprobe even if the seat never reported a transition.
    pub fn request_refresh(&mut self) {
        self.invalidate();
    }

    pub fn can_begin(&self) -> bool {
        self.active && self.original_present && self.pending && self.in_flight.is_none()
    }

    /// Caller must separately validate the actual original KMS FD before
    /// starting any worker. At most one worker and one pending request exist.
    pub fn begin(&mut self, now: Instant, original_fd_valid: bool) -> Option<Ticket> {
        if !self.active
            || !self.original_present
            || !original_fd_valid
            || !self.pending
            || self.in_flight.is_some()
        {
            return None;
        }
        let ticket = Ticket {
            device: self.device,
            epoch: self.epoch,
            started: now,
        };
        self.pending = false;
        self.attempts += 1;
        self.in_flight = Some(ticket);
        Some(ticket)
    }

    /// Admission predicate used before serialized resource reconciliation.
    pub fn accepts(&self, ticket: Ticket, now: Instant, original_fd_valid: bool) -> bool {
        self.in_flight == Some(ticket)
            && self.active
            && self.original_present
            && original_fd_valid
            && ticket.device == self.device
            && ticket.epoch == self.epoch
            && now
                .checked_duration_since(ticket.started)
                .is_some_and(|age| age <= REFRESH_DEADLINE)
    }

    /// start(Busy) did not acquire a worker; preserve its pending request and
    /// finite acquisition retry budget until the process-wide slot is free.
    pub fn deferred_busy(&mut self, ticket: Ticket) -> bool {
        if self.in_flight != Some(ticket) {
            return false;
        }
        self.in_flight = None;
        if ticket.epoch == self.epoch {
            self.attempts = self.attempts.saturating_sub(1);
        }
        self.pending = self.original_present;
        true
    }

    /// Call only when the worker really returned; a timer cannot release its
    /// slot. A forged/stale completion cannot release another worker's slot.
    /// Acceptance additionally requires original FD/owner guards at completion.
    pub fn completed(
        &mut self,
        ticket: Ticket,
        now: Instant,
        original_fd_valid: bool,
        acquisition_succeeded: bool,
    ) -> Completion {
        if self.in_flight != Some(ticket) {
            return Completion::Foreign;
        }
        self.in_flight = None;
        let current = self.active
            && self.original_present
            && original_fd_valid
            && ticket.device == self.device
            && ticket.epoch == self.epoch;
        let fresh = now
            .checked_duration_since(ticket.started)
            .is_some_and(|age| age <= REFRESH_DEADLINE);
        if current && fresh && acquisition_succeeded {
            self.validated_epoch = Some(self.epoch);
            return Completion::Accepted;
        }
        // A newer event already supplied its own single pending request. A
        // failed current request gets one recovery attempt, never infinite
        // timer retries or additional workers for a blocked kernel read.
        if current && self.attempts < 2 {
            self.pending = true;
        }
        if self.pending && self.active && self.original_present {
            Completion::RetryPending
        } else {
            Completion::Unavailable
        }
    }

    pub fn returned(&mut self, ticket: Ticket, now: Instant, original_fd_valid: bool) -> bool {
        self.completed(ticket, now, original_fd_valid, true) == Completion::Accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn other_gpu_cannot_invalidate_or_schedule_original() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        let startup = state.begin(now, true).unwrap();
        assert_eq!(
            state.completed(startup, now, true, true),
            Completion::Accepted
        );
        assert!(!state.device_event(8, DeviceEvent::Changed));
        assert!(state.begin(now, true).is_none());
    }
    #[test]
    fn storm_coalesces_and_old_completion_cannot_admit_metadata() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        let old = state.begin(now, true).unwrap();
        for _ in 0..1000 {
            state.device_event(7, DeviceEvent::Changed);
        }
        assert!(state.begin(now, true).is_none());
        assert!(!state.returned(old, now, true));
        let latest = state.begin(now, true).unwrap();
        assert!(latest.epoch > old.epoch);
        assert!(state.returned(latest, now, true));
        assert!(state.begin(now, true).is_none());
    }
    #[test]
    fn seat_pause_defers_and_revokes_original_observation() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        let old = state.begin(now, true).unwrap();
        assert!(state.set_active(false));
        assert!(!state.returned(old, now, true));
        assert!(state.begin(now, true).is_none());
        assert!(state.set_active(true));
        assert!(state.begin(now, true).is_some());
    }
    #[test]
    fn removal_and_reused_device_number_do_not_resurrect_authority() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        let old = state.begin(now, true).unwrap();
        state.device_event(7, DeviceEvent::Removed);
        state.device_event(7, DeviceEvent::Added);
        assert!(!state.returned(old, now, true));
        assert!(state.begin(now, true).is_none());
    }
    #[test]
    fn expired_worker_keeps_slot_until_actual_return() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        let old = state.begin(now, true).unwrap();
        state.request_refresh();
        let late = now + REFRESH_DEADLINE + Duration::from_nanos(1);
        assert!(state.begin(late, true).is_none());
        assert!(!state.returned(old, late, true));
        assert!(state.begin(late, true).is_some());
    }
    #[test]
    fn exact_deadline_backward_clock_and_original_fd_fail_closed() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        assert!(state.begin(now, false).is_none());
        let ticket = state.begin(now, true).unwrap();
        assert!(state.returned(ticket, now + REFRESH_DEADLINE, true));
        state.request_refresh();
        let ticket = state.begin(now, true).unwrap();
        assert!(!state.returned(ticket, now - Duration::from_nanos(1), true));
        state.request_refresh();
        let ticket = state.begin(now, true).unwrap();
        assert!(!state.returned(ticket, now, false));
    }
    #[test]
    fn final_probe_latency_never_reuses_preprobe_admission_time() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        let ticket = state.begin(now, true).unwrap();
        assert!(state.accepts(ticket, now, true));
        let after_probe = now + REFRESH_DEADLINE + Duration::from_nanos(1);
        assert!(!state.accepts(ticket, after_probe, true));
        assert_eq!(
            state.completed(ticket, after_probe, true, true),
            Completion::RetryPending
        );
        assert_eq!(state.ownership_generation(), None);
    }

    #[test]
    fn busy_orphan_does_not_burn_acquisition_retry_budget() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        for _ in 0..1000 {
            let ticket = state.begin(now, true).unwrap();
            assert!(state.deferred_busy(ticket));
            assert_eq!(state.attempts, 0);
            assert_eq!(state.ownership_generation(), None);
        }
        let ticket = state.begin(now, true).unwrap();
        let mut wrong = ticket;
        wrong.device = 8;
        assert!(!state.deferred_busy(wrong));
        assert_eq!(
            state.completed(ticket, now, true, false),
            Completion::RetryPending
        );
        let ticket = state.begin(now, true).unwrap();
        assert_eq!(
            state.completed(ticket, now, true, false),
            Completion::Unavailable
        );
    }

    #[test]
    fn discovery_generation_revoked_until_actual_current_completion() {
        let mut state = RefreshState::new(7);
        assert_eq!(state.ownership_generation(), None);
        let now = Instant::now();
        let startup = state.begin(now, true).unwrap();
        assert_eq!(startup.epoch, 1);
        assert_eq!(
            state.completed(startup, now, true, true),
            Completion::Accepted
        );
        assert_eq!(state.ownership_generation(), Some(1));
        state.device_event(7, DeviceEvent::Changed);
        assert_eq!(state.ownership_generation(), None);
        let ticket = state.begin(now, true).unwrap();
        assert_eq!(state.ownership_generation(), None);
        assert_eq!(
            state.completed(ticket, now, true, false),
            Completion::RetryPending
        );
        assert_eq!(state.ownership_generation(), None);
        let ticket = state.begin(now, true).unwrap();
        assert_eq!(
            state.completed(ticket, now, true, true),
            Completion::Accepted
        );
        assert_eq!(state.ownership_generation(), Some(ticket.epoch));
        state.set_active(false);
        assert_eq!(state.ownership_generation(), None);
    }

    #[test]
    fn transient_failure_has_one_recovery_then_explicit_unavailability() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        let ticket = state.begin(now, true).unwrap();
        assert_eq!(
            state.completed(ticket, now, true, false),
            Completion::RetryPending
        );
        let retry = state.begin(now, true).unwrap();
        assert_eq!(
            state.completed(retry, now, true, false),
            Completion::Unavailable
        );
        assert!(state.begin(now, true).is_none());
        state.device_event(7, DeviceEvent::Changed);
        let next = state.begin(now, true).unwrap();
        assert_eq!(state.completed(next, now, true, true), Completion::Accepted);
    }

    #[test]
    fn wrong_ticket_cannot_free_an_owned_slot_and_epoch_never_wraps() {
        let mut state = RefreshState::new(7);
        let now = Instant::now();
        state.request_refresh();
        let ticket = state.begin(now, true).unwrap();
        let mut wrong = ticket;
        wrong.device = 8;
        assert!(!state.returned(wrong, now, true));
        assert!(state.begin(now, true).is_none());
        assert!(state.returned(ticket, now, true));
        state.epoch = u64::MAX;
        state.request_refresh();
        assert!(state.begin(now, true).is_none());
    }
}

#[cfg(test)]
mod notifier_fault_tests {
    use super::*;
    #[test]
    fn repeated_errors_cannot_renew_original_epoch_or_finite_attempts() {
        let now = Instant::now();
        let mut state = RefreshState::new(226);
        let mut fault = EventFault::default();
        let original = state.begin(now, true).unwrap();
        assert_eq!(
            state.completed(original, now, true, true),
            Completion::Accepted
        );
        assert!(fault.revoke_once());
        state.request_refresh();
        assert!(state.ownership_generation().is_none());
        assert!(!state.accepts(original, now, true));
        let first = state.begin(now, true).unwrap();
        assert!(!fault.revoke_once());
        assert_eq!(
            state.completed(first, now, true, false),
            Completion::RetryPending
        );
        let last = state.begin(now, true).unwrap();
        assert!(!fault.revoke_once());
        assert_eq!(
            state.completed(last, now, true, false),
            Completion::Unavailable
        );
        assert!(state.begin(now, true).is_none());
        assert!(!fault.revoke_once());
        // Successful matching callback is a separate actual caller fact; stale
        // callbacks never reach this method in the production consumer.
        fault.matched_completion();
        assert!(fault.revoke_once());
    }
}
