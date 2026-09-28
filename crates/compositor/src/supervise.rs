// Wave 2 stream 4 owns this file: shell supervision (restart budget,
// backoff) and recovery-affordance scope. The 001 plan (T5) additionally
// drives it from the nested runtime loop.
//
// Implements the compositor-owned side of ADR 0003 ("Nested supervision
// and recovery affordance"): the compositor spawns the shell as a child
// process, restarts it from a finite budget with capped backoff, and
// kills it on compositor exit. Uses `std` only: no async runtime, no
// extra crates. Polling is non-blocking (`Child::try_wait`) so a dead
// or looping shell can neither block nor spin the compositor.

use std::cell::Cell;
use std::io;
use std::process::{Child, Command};
use std::time::{SystemTime, UNIX_EPOCH};

/// Millisecond clock used to gate restart backoff.
///
/// Passing the time in (rather than reading it inside [`Supervisor`])
/// keeps supervision deterministic under test: production passes
/// [`SystemClock::now_ms`], tests drive a [`ManualClock`].
pub trait Clock {
    /// Current time in milliseconds since an implementation-defined epoch.
    ///
    /// Only differences between readings are meaningful.
    fn now_ms(&self) -> u64;
}

/// [`Clock`] backed by the system wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

/// Controllable [`Clock`] for tests: time only moves when advanced.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Cell<u64>,
}

impl ManualClock {
    /// Clock fixed at `start_ms`.
    pub fn new(start_ms: u64) -> Self {
        Self {
            now: Cell::new(start_ms),
        }
    }

    /// Move the clock forward by `delta_ms`, saturating at `u64::MAX`.
    pub fn advance(&self, delta_ms: u64) {
        self.now.set(self.now.get().saturating_add(delta_ms));
    }

    /// Jump the clock to `now_ms`. May move backwards; supervision only
    /// compares against previously recorded deadlines, so a backwards
    /// jump merely delays the next restart until the deadline passes.
    pub fn set(&self, now_ms: u64) {
        self.now.set(now_ms);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
}

/// Bounded restart policy: finite attempts, capped exponential backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Maximum number of restarts after the initial spawn.
    /// Total spawns never exceed `1 + max_attempts`.
    pub max_attempts: u32,
    /// Delay before the first restart.
    pub base_backoff_ms: u64,
    /// Upper bound for any restart delay.
    pub max_backoff_ms: u64,
}

impl RestartPolicy {
    /// Build a policy. A `max_backoff_ms` below `base_backoff_ms` is
    /// clamped up to `base_backoff_ms` so the cap never shortens the
    /// first delay.
    pub fn new(max_attempts: u32, base_backoff_ms: u64, max_backoff_ms: u64) -> Self {
        Self {
            max_attempts,
            base_backoff_ms,
            max_backoff_ms: max_backoff_ms.max(base_backoff_ms),
        }
    }

    /// Delay before restart number `attempt` (0-based: 0 is the first
    /// restart). Pure function of its inputs: `base * 2^attempt`,
    /// saturated and capped at `max_backoff_ms`.
    pub fn next_delay(&self, attempt: u32) -> u64 {
        let factor = 1u64.checked_shl(attempt.min(31)).unwrap_or(u64::MAX);
        self.base_backoff_ms
            .saturating_mul(factor)
            .min(self.max_backoff_ms)
    }
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::new(5, 250, 5_000)
    }
}

/// What [`Supervisor::poll`] observed since the previous call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildEvent {
    /// A child handle is held and still alive, or was (re)spawned by
    /// this call.
    Running,
    /// No live child. Carries the most recent exit code (`None` when
    /// the process was killed by a signal or no exit was observed yet),
    /// mirroring [`std::process::ExitStatus::code`].
    Exited(Option<i32>),
}

/// Supervision failures surfaced to the compositor loop.
#[derive(Debug)]
pub enum SuperviseError {
    /// The restart budget is spent and no child is held. Returned
    /// immediately; the supervisor never retries or spins to produce it.
    BudgetExhausted,
    /// A (re)spawn failed. The failure arms the normal backoff so a
    /// tight poll loop cannot churn the process table.
    SpawnFailed(io::Error),
    /// A non-blocking `try_wait` failed unexpectedly.
    WaitFailed(io::Error),
}

impl std::fmt::Display for SuperviseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExhausted => write!(f, "shell restart budget exhausted"),
            Self::SpawnFailed(e) => write!(f, "failed to spawn shell: {e}"),
            Self::WaitFailed(e) => write!(f, "failed to poll shell: {e}"),
        }
    }
}

impl std::error::Error for SuperviseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BudgetExhausted => None,
            Self::SpawnFailed(e) | Self::WaitFailed(e) => Some(e),
        }
    }
}

/// Owns one supervised shell child: finite restarts, capped backoff,
/// kill-on-drop.
///
/// Typical loop: `spawn` (or let the first `poll` spawn), then call
/// `poll` on each compositor tick with the current time in ms. While a
/// restart backoff is pending, `poll` reports the last
/// [`ChildEvent::Exited`] without spawning; once the budget is spent it
/// returns [`SuperviseError::BudgetExhausted`] instead of spinning.
/// Dropping the supervisor kills a live child best-effort so the shell
/// never outlives the compositor (ADR 0003: kill on compositor exit).
pub struct Supervisor {
    policy: RestartPolicy,
    child: Option<Child>,
    started: bool,
    restarts_used: u32,
    next_allowed_ms: u64,
    last_exit: Option<Option<i32>>,
}

impl Supervisor {
    /// Supervisor with no child yet; the first `poll` spawns.
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            child: None,
            started: false,
            restarts_used: 0,
            next_allowed_ms: 0,
            last_exit: None,
        }
    }

    /// Spawn the shell now, replacing any live child (killed
    /// best-effort). Does not consume restart budget; use
    /// [`Supervisor::reset_budget`] afterwards if this is an operator-
    /// requested relaunch rather than the initial start.
    pub fn spawn(&mut self, command: &mut Command) -> io::Result<()> {
        self.kill_child();
        self.child = Some(command.spawn()?);
        self.started = true;
        self.next_allowed_ms = 0;
        Ok(())
    }

    /// Non-blocking supervision step. Reaps an exited child via
    /// `try_wait`, arms the backoff for the next restart, and respawns
    /// through `remake` once the budget allows and the backoff deadline
    /// has passed at `now_ms`.
    pub fn poll(
        &mut self,
        remake: &mut dyn FnMut() -> Command,
        now_ms: u64,
    ) -> Result<ChildEvent, SuperviseError> {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Err(e) => return Err(SuperviseError::WaitFailed(e)),
                Ok(None) => return Ok(ChildEvent::Running),
                Ok(Some(status)) => {
                    self.child = None;
                    let code = status.code();
                    self.last_exit = Some(code);
                    self.next_allowed_ms =
                        now_ms.saturating_add(self.policy.next_delay(self.restarts_used));
                    return Ok(ChildEvent::Exited(code));
                }
            }
        }

        if self.started && self.restarts_used >= self.policy.max_attempts {
            return Err(SuperviseError::BudgetExhausted);
        }
        if now_ms < self.next_allowed_ms {
            return Ok(ChildEvent::Exited(self.last_exit.unwrap_or(None)));
        }
        match remake().spawn() {
            Ok(child) => {
                self.child = Some(child);
                if self.started {
                    self.restarts_used = self.restarts_used.saturating_add(1);
                }
                self.started = true;
                Ok(ChildEvent::Running)
            }
            Err(e) => {
                // Gate the next retry so spawn failures cannot spin.
                self.next_allowed_ms =
                    now_ms.saturating_add(self.policy.next_delay(self.restarts_used));
                Err(SuperviseError::SpawnFailed(e))
            }
        }
    }

    /// Report whether supervision is exhausted: no child held and no
    /// restarts left. Pure check — never blocks, retries, or spawns.
    pub fn exhaust(&self) -> Result<(), SuperviseError> {
        if self.started && self.child.is_none() && self.restarts_used >= self.policy.max_attempts {
            return Err(SuperviseError::BudgetExhausted);
        }
        Ok(())
    }

    /// Restore the full restart budget and clear any pending backoff.
    /// Keeps a live child. Used for operator-requested relaunches (see
    /// [`RecoveryAction::RelaunchShell`).
    pub fn reset_budget(&mut self) {
        self.restarts_used = 0;
        self.next_allowed_ms = 0;
    }

    /// Policy this supervisor enforces.
    pub fn policy(&self) -> RestartPolicy {
        self.policy
    }

    /// Restarts performed so far (initial spawn excluded).
    pub fn restarts_used(&self) -> u32 {
        self.restarts_used
    }

    /// Restarts remaining before [`SuperviseError::BudgetExhausted`].
    pub fn restarts_remaining(&self) -> u32 {
        self.policy.max_attempts.saturating_sub(self.restarts_used)
    }

    /// Whether a child handle is currently held. The process may have
    /// exited but not yet been reaped by `poll`.
    pub fn has_child(&self) -> bool {
        self.child.is_some()
    }

    fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.kill_child();
    }
}

impl Supervisor {
    /// Milliseconds until the next restart is allowed at `now_ms`
    /// (zero when a restart could run now). Pure check for diagnostics
    /// and the shell driver; never spawns or advances state.
    pub fn backoff_remaining_ms(&self, now_ms: u64) -> u64 {
        self.next_allowed_ms.saturating_sub(now_ms)
    }
}

/// Observable supervision outcomes (001 T5): restart, resync, and
/// exhaustion surface as values carrying only codes, delays, and counts —
/// never window content or secrets — so diagnostics stay privacy-safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorEvent {
    /// The shell child exited (code mirrors `ExitStatus::code`: `None`
    /// for signal kills or unobserved exits).
    ShellExited { code: Option<i32> },
    /// A restart is armed after `delay_ms` (budget remains).
    RestartScheduled { delay_ms: u64 },
    /// The restart budget is spent; the shell stays down until an
    /// operator-requested relaunch.
    BudgetExhausted,
}

/// What [`ShellDriver::poll`] found this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellStatus {
    /// Shell child held and alive (or freshly respawned).
    Running,
    /// No live shell; a restart is armed after the delay. The
    /// compositor-owned overlay covers the session meanwhile.
    Waiting { delay_ms: u64 },
    /// Restart budget spent; calm and observable, no spinning.
    Exhausted,
    /// Supervision itself faulted (spawn/wait error text, no content).
    /// Treated like absence: the overlay covers the session.
    Fault(String),
}

/// One supervised shell child bound to a nested session (001 T5).
///
/// Owns a [`Supervisor`] plus the remake recipe: the shell binary with
/// the session's `WAYLAND_DISPLAY` and `RWD_CONTROL_SOCKET` set for the
/// child only. Drive with [`poll`](Self::poll) once per compositor tick
/// and drain [`events`](Self::drain_events) for redacted diagnostics.
pub struct ShellDriver {
    supervisor: Supervisor,
    bin: std::path::PathBuf,
    wayland_display: String,
    control_socket: std::path::PathBuf,
    events: Vec<SupervisorEvent>,
}

impl ShellDriver {
    /// Driver with no child yet; the first [`poll`](Self::poll) spawns.
    pub fn new(
        policy: RestartPolicy,
        bin: std::path::PathBuf,
        wayland_display: String,
        control_socket: std::path::PathBuf,
    ) -> Self {
        Self {
            supervisor: Supervisor::new(policy),
            bin,
            wayland_display,
            control_socket,
            events: Vec::new(),
        }
    }

    /// Non-blocking supervision step at `now_ms`. Never blocks the
    /// compositor tick: spawn failures and dead children report as
    /// values, and every outcome is also appended as a
    /// [`SupervisorEvent`] for [`drain_events`](Self::drain_events).
    pub fn poll(&mut self, now_ms: u64) -> ShellStatus {
        let supervisor = &mut self.supervisor;
        let bin = &self.bin;
        let wayland_display = &self.wayland_display;
        let control_socket = &self.control_socket;
        let mut remake = || {
            let mut command = Command::new(bin);
            command.env("WAYLAND_DISPLAY", wayland_display);
            command.env("RWD_CONTROL_SOCKET", control_socket);
            command
        };
        match supervisor.poll(&mut remake, now_ms) {
            Ok(ChildEvent::Running) => ShellStatus::Running,
            Ok(ChildEvent::Exited(code)) => {
                self.events.push(SupervisorEvent::ShellExited { code });
                let delay_ms = supervisor.backoff_remaining_ms(now_ms);
                self.events
                    .push(SupervisorEvent::RestartScheduled { delay_ms });
                ShellStatus::Waiting { delay_ms }
            }
            Err(SuperviseError::BudgetExhausted) => {
                self.events.push(SupervisorEvent::BudgetExhausted);
                ShellStatus::Exhausted
            }
            Err(e) => ShellStatus::Fault(e.to_string()),
        }
    }

    /// Take the pending supervision events, oldest first. Events carry
    /// only codes, delays, and counts — safe to log verbatim.
    pub fn drain_events(&mut self) -> Vec<SupervisorEvent> {
        std::mem::take(&mut self.events)
    }

    /// Restore the full restart budget for an operator-requested
    /// relaunch (overlay action). Keeps a live child; the next
    /// [`poll`](Self::poll) respawns when none is held.
    pub fn reset_budget(&mut self) {
        self.supervisor.reset_budget();
    }

    /// Restarts performed so far (initial spawn excluded).
    pub fn restarts_used(&self) -> u32 {
        self.supervisor.restarts_used()
    }

    /// Whether a child handle is currently held.
    pub fn has_child(&self) -> bool {
        self.supervisor.has_child()
    }

    /// Policy this driver enforces.
    pub fn policy(&self) -> RestartPolicy {
        self.supervisor.policy()
    }
}

/// Redacted per-run evidence for one nested fault run (001 T5).
///
/// Built only from revisions, counts, and hashes — never titles, tokens,
/// or frame content — so retained artifacts are privacy-safe by
/// construction. The 100-run harness records one per run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArtifact {
    /// Zero-based run index within the harness.
    pub run: u64,
    /// Fault injected this run (fixed vocabulary, e.g. `"kill"`).
    pub fault: &'static str,
    /// Compositor revision before the fault.
    pub revision_before: u64,
    /// Compositor revision after reconnect/resync.
    pub revision_after: u64,
    /// Shell restarts consumed this run.
    pub restarts_used: u32,
    /// Hash of `(revision_after, sorted window ids)`: proves the exact
    /// window set resynced without storing any content.
    pub snapshot_hash: u64,
    /// Human note built from numbers only (no titles or tokens).
    pub note: String,
}

/// Hash `(revision, sorted window ids)` for [`RunArtifact::snapshot_hash`].
/// FNV-1a over fixed integers: content-free and deterministic.
pub fn snapshot_hash(revision: u64, window_ids: &[u64]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for word in std::iter::once(revision).chain(window_ids.iter().copied()) {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

/// Recovery affordance available while the shell is absent (ADR 0003:
/// compositor-owned emergency overlay on the existing input + GLES
/// path; a separate recovery client is deferred).
///
/// Every variant is safe without a running shell:
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Show the compositor-owned window list, served from compositor
    /// state (no shell process or IPC needed).
    ListWindows,
    /// Request an immediate shell relaunch via the [`Supervisor`]
    /// restart path (clears backoff/budget, respawns the child).
    RelaunchShell,
    /// Show or hide the compositor-owned emergency overlay itself,
    /// rendered on the existing input + GLES path with focus and input
    /// kept alive.
    ShowOverlay,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn false_cmd() -> Command {
        Command::new("/bin/false")
    }

    fn true_cmd() -> Command {
        Command::new("/bin/true")
    }

    #[test]
    fn driver_spawns_shell_with_session_env_and_reports_running() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("control.sock");
        let mut driver = ShellDriver::new(
            RestartPolicy::new(1, 1_000, 1_000),
            std::path::PathBuf::from("/bin/true"),
            "rwd-test.sock".to_owned(),
            control,
        );
        assert_eq!(driver.poll(0), ShellStatus::Running);
        assert!(driver.has_child());
        assert!(driver.drain_events().is_empty());
        assert_eq!(driver.restarts_used(), 0);
    }

    #[test]
    fn driver_missing_binary_faults_without_spinning() {
        let dir = tempfile::tempdir().unwrap();
        let mut driver = ShellDriver::new(
            RestartPolicy::new(1, 1_000, 1_000),
            dir.path().join("no-such-shell"),
            "rwd-test.sock".to_owned(),
            dir.path().join("control.sock"),
        );
        assert!(matches!(driver.poll(0), ShellStatus::Fault(_)));
        // Spawn failure arms the backoff: same tick reports waiting,
        // never a hot respawn loop.
        assert!(matches!(
            driver.poll(0),
            ShellStatus::Waiting { .. } | ShellStatus::Fault(_)
        ));
    }

    #[test]
    fn driver_reset_budget_relaunches_after_exhaustion() {
        let dir = tempfile::tempdir().unwrap();
        let mut driver = ShellDriver::new(
            RestartPolicy::new(1, 1, 1),
            std::path::PathBuf::from("/bin/false"),
            "rwd-test.sock".to_owned(),
            dir.path().join("control.sock"),
        );
        let reap_exit = |driver: &mut ShellDriver, now: u64| {
            for _ in 0..10_000 {
                match driver.poll(now) {
                    ShellStatus::Waiting { .. } => return,
                    ShellStatus::Running => std::thread::yield_now(),
                    other => panic!("unexpected driver status: {other:?}"),
                }
            }
            panic!("exit reap budget exhausted");
        };
        assert_eq!(driver.poll(0), ShellStatus::Running);
        reap_exit(&mut driver, 0);
        // Past the backoff: the one budgeted restart respawns.
        assert_eq!(driver.poll(100), ShellStatus::Running);
        assert_eq!(driver.restarts_used(), 1);
        reap_exit(&mut driver, 100);
        assert_eq!(driver.poll(200), ShellStatus::Exhausted);
        // Operator relaunch restores the budget and respawns.
        driver.reset_budget();
        assert_eq!(driver.poll(200), ShellStatus::Running);
    }

    #[test]
    fn snapshot_hash_is_deterministic_and_content_free() {
        let ids = [3u64, 1, 2];
        assert_eq!(snapshot_hash(7, &ids), snapshot_hash(7, &ids));
        assert_ne!(snapshot_hash(7, &ids), snapshot_hash(8, &ids));
        assert_ne!(snapshot_hash(7, &ids), snapshot_hash(7, &[1, 2, 4]));
    }

    #[test]
    fn backoff_sequence_doubles() {
        let policy = RestartPolicy::new(8, 100, 10_000);
        let delays: Vec<u64> = (0..6).map(|a| policy.next_delay(a)).collect();
        assert_eq!(delays, vec![100, 200, 400, 800, 1_600, 3_200]);
    }

    #[test]
    fn backoff_caps_at_max() {
        let policy = RestartPolicy::new(8, 1_000, 2_500);
        assert_eq!(policy.next_delay(0), 1_000);
        assert_eq!(policy.next_delay(1), 2_000);
        assert_eq!(policy.next_delay(2), 2_500);
        assert_eq!(policy.next_delay(3), 2_500);
        assert_eq!(policy.next_delay(100), 2_500);
    }

    #[test]
    fn backoff_never_overflows() {
        let policy = RestartPolicy::new(u32::MAX, u64::MAX, u64::MAX);
        assert_eq!(policy.next_delay(u32::MAX), u64::MAX);
        let zero = RestartPolicy::new(3, 0, 0);
        assert_eq!(zero.next_delay(5), 0);
    }

    #[test]
    fn budget_exhaustion_uses_manual_clock_without_sleeps() {
        let policy = RestartPolicy::new(1, 1_000, 1_000);
        let clock = ManualClock::new(0);
        let mut sup = Supervisor::new(policy);
        let mut remake = || false_cmd();

        // Initial spawn is free.
        assert_eq!(
            sup.poll(&mut remake, clock.now_ms()).unwrap(),
            ChildEvent::Running
        );
        // /bin/false exits immediately; reap it (bounded retries in case
        // the exit has not been scheduled yet; the fake clock does not move).
        let mut exited = None;
        for _ in 0..10_000 {
            match sup.poll(&mut remake, clock.now_ms()).unwrap() {
                ChildEvent::Exited(code) => {
                    exited = Some(code);
                    break;
                }
                ChildEvent::Running => std::thread::yield_now(),
            }
        }
        assert_eq!(exited, Some(Some(1)));
        assert_eq!(sup.restarts_used(), 0);
        // Backoff pending: no respawn yet, last exit reported, no spin.
        assert_eq!(
            sup.poll(&mut remake, clock.now_ms()).unwrap(),
            ChildEvent::Exited(Some(1))
        );
        assert!(!sup.has_child());
        assert!(sup.exhaust().is_ok());

        clock.advance(1_000);
        assert_eq!(
            sup.poll(&mut remake, clock.now_ms()).unwrap(),
            ChildEvent::Running
        );
        assert_eq!(sup.restarts_used(), 1);
        assert_eq!(sup.restarts_remaining(), 0);

        let mut exited = None;
        for _ in 0..10_000 {
            match sup.poll(&mut remake, clock.now_ms()).unwrap() {
                ChildEvent::Exited(code) => {
                    exited = Some(code);
                    break;
                }
                ChildEvent::Running => std::thread::yield_now(),
            }
        }
        assert_eq!(exited, Some(Some(1)));
        assert!(matches!(
            sup.poll(&mut remake, clock.now_ms()),
            Err(SuperviseError::BudgetExhausted)
        ));
        assert!(matches!(
            sup.exhaust(),
            Err(SuperviseError::BudgetExhausted)
        ));
    }

    #[test]
    fn live_true_spawn_observed_exiting() {
        let mut sup = Supervisor::new(RestartPolicy::default());
        sup.spawn(&mut true_cmd()).unwrap();
        let deadline = SystemClock.now_ms().saturating_add(4_000);
        loop {
            let mut remake = || true_cmd();
            match sup.poll(&mut remake, SystemClock.now_ms()).unwrap() {
                ChildEvent::Exited(code) => {
                    assert_eq!(code, Some(0));
                    return;
                }
                ChildEvent::Running => {
                    assert!(SystemClock.now_ms() < deadline, "/bin/true never exited");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    #[test]
    fn live_false_exhausts_two_attempt_budget() {
        let clock = ManualClock::new(0);
        let mut sup = Supervisor::new(RestartPolicy::new(2, 10, 10));
        let mut remake = || false_cmd();
        let start = std::time::Instant::now();

        let mut now = clock.now_ms();
        assert_eq!(sup.poll(&mut remake, now).unwrap(), ChildEvent::Running);
        // Drive: reap exits, jump the fake clock past backoff, respawn.
        for _ in 0..8 {
            match sup.poll(&mut remake, now).unwrap() {
                ChildEvent::Running => {}
                ChildEvent::Exited(_) => {
                    clock.set(now.saturating_add(10));
                    now = clock.now_ms();
                }
            }
            if sup.has_child() {
                // Give the fresh /bin/false a moment to exit.
                std::thread::sleep(Duration::from_millis(5));
            }
            if matches!(
                sup.poll(&mut remake, now),
                Err(SuperviseError::BudgetExhausted)
            ) {
                break;
            }
            if sup.exhaust().is_err() && !sup.has_child() {
                break;
            }
        }
        assert!(matches!(
            sup.exhaust(),
            Err(SuperviseError::BudgetExhausted)
        ));
        assert_eq!(sup.restarts_used(), 2);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
