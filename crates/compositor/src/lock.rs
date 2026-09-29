//! Session lock flag and idle timeout (session-lock, flag task).
//!
//! Pure logic only: the locked flag, the idle timeout measured from
//! input timestamps, and the present-content decision. No rendering
//! here — the runtime owns the input clock bridge, the overlay surface,
//! and the frame path, and drives this state machine.
//!
//! The flag lives in the compositor, never the shell: the shell only
//! sees the `locked` bit on snapshots. Time is caller-supplied
//! milliseconds in the input-timestamp base (`ManagerInput::time`);
//! the runtime bridges wall-clock idle onto that base (see
//! [`SessionLock::check_timeout`]), so this machine never mixes the
//! backend event clock with the system clock itself.

/// Default idle timeout: five minutes without input locks the session.
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;

/// Compositor-owned session lock: idle accumulator plus flag.
///
/// `last_input_ms` is the newest input timestamp seen; idle time is
/// `now_ms - last_input_ms` with saturation, so reordered or skewed
/// stamps can delay but never force a lock. Any input resets the
/// accumulator; only [`check_timeout`](Self::check_timeout),
/// [`lock`](Self::lock), or [`unlock`](Self::unlock) change the flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLock {
    locked: bool,
    last_input_ms: u64,
    timeout_ms: u64,
}

impl SessionLock {
    /// Unlocked machine that locks after `timeout_ms` without input.
    /// `last_input_ms` starts at zero (session boot in the input base).
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            locked: false,
            last_input_ms: 0,
            timeout_ms,
        }
    }

    /// Whether the session is currently locked.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Newest input timestamp seen, in the input base.
    pub fn last_input_ms(&self) -> u64 {
        self.last_input_ms
    }

    /// Idle timeout in milliseconds.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Replace the idle timeout. Applies to the next
    /// [`check_timeout`](Self::check_timeout) call.
    pub fn set_timeout(&mut self, timeout_ms: u64) {
        self.timeout_ms = timeout_ms;
    }

    /// Record one input event timestamp. Moves the accumulator forward
    /// only: stale or reordered stamps are ignored. Never (un)locks.
    pub fn note_input(&mut self, timestamp_ms: u64) {
        self.last_input_ms = self.last_input_ms.max(timestamp_ms);
    }

    /// Idle milliseconds at `now_ms` in the input base (saturating).
    pub fn idle_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.last_input_ms)
    }

    /// Lock when the idle timeout has elapsed. Returns true only on
    /// the transition (false when already locked or still within
    /// budget). A zero timeout locks on the first check at or past
    /// the last input stamp.
    pub fn check_timeout(&mut self, now_ms: u64) -> bool {
        if !self.locked && self.idle_ms(now_ms) >= self.timeout_ms {
            self.locked = true;
            return true;
        }
        false
    }

    /// Engage the lock at once (control-command set path). Idempotent.
    pub fn lock(&mut self) {
        self.locked = true;
    }

    /// Clear the lock (session-auth path, owned by the unlock task)
    /// and restart the idle accumulator at `now_ms`, so the session
    /// does not relock on the next check.
    pub fn unlock(&mut self, now_ms: u64) {
        self.locked = false;
        self.last_input_ms = self.last_input_ms.max(now_ms);
    }
}

/// Whether window and layer content may be presented this frame.
///
/// While locked nothing beneath the lock surface may show: no window
/// pixels, no titles, no layer-shell chrome (panel, notifications),
/// no wallpaper. The recovery overlay keeps its own provisional look
/// and still shows content behind it; only the lock hides.
pub fn content_visible(locked: bool) -> bool {
    !locked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_unlocked_with_zero_idle() {
        let lock = SessionLock::new(1_000);
        assert!(!lock.is_locked());
        assert_eq!(lock.last_input_ms(), 0);
        assert_eq!(lock.idle_ms(0), 0);
    }

    #[test]
    fn timeout_engages_once() {
        let mut lock = SessionLock::new(1_000);
        lock.note_input(500);
        assert!(!lock.check_timeout(1_499));
        assert!(lock.check_timeout(1_500));
        assert!(lock.is_locked());
        // Already locked: no second transition.
        assert!(!lock.check_timeout(99_000));
    }

    #[test]
    fn activity_resets_the_accumulator() {
        let mut lock = SessionLock::new(1_000);
        lock.note_input(0);
        assert!(!lock.check_timeout(900));
        lock.note_input(900);
        assert!(!lock.check_timeout(1_800));
        assert!(lock.check_timeout(1_900));
    }

    #[test]
    fn stale_timestamps_never_move_the_accumulator_back() {
        let mut lock = SessionLock::new(1_000);
        lock.note_input(5_000);
        lock.note_input(100);
        assert_eq!(lock.last_input_ms(), 5_000);
        // `now` behind the last stamp saturates to zero idle.
        assert_eq!(lock.idle_ms(4_000), 0);
        assert!(!lock.check_timeout(4_000));
    }

    #[test]
    fn control_lock_engages_at_once_and_is_idempotent() {
        let mut lock = SessionLock::new(u64::MAX);
        lock.lock();
        assert!(lock.is_locked());
        lock.lock();
        assert!(lock.is_locked());
    }

    #[test]
    fn unlock_clears_and_restarts_idle() {
        let mut lock = SessionLock::new(1_000);
        lock.note_input(0);
        assert!(lock.check_timeout(5_000));
        lock.unlock(5_000);
        assert!(!lock.is_locked());
        // Fresh budget from the unlock stamp.
        assert!(!lock.check_timeout(5_999));
        assert!(lock.check_timeout(6_000));
    }

    #[test]
    fn zero_timeout_locks_on_first_check() {
        let mut lock = SessionLock::new(0);
        assert!(lock.check_timeout(0));
    }

    #[test]
    fn content_hidden_only_while_locked() {
        assert!(content_visible(false));
        assert!(!content_visible(true));
    }
}
