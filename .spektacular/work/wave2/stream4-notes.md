# Stream 4 notes — shell supervision (`supervise.rs`)

## Reference survey (behavior level, no verbatim code reused)

### niri (clone at `/tmp/niri-ref`, depth-1, Sep 2026)

niri does **not** supervise session components in-compositor. Supervision is
delegated to systemd; the compositor only fire-and-forgets:

- `src/utils/spawning.rs:66` — `pub fn spawn(...)` documented "Spawns the
  command to run independently of the compositor."
- `src/utils/spawning.rs:176-178` — `do_spawn` double-forks "to avoid having
  to waitpid the child", i.e. deliberately orphans children so there is no
  child to reap, wait on, or restart.
- `src/utils/spawning.rs:159-163` — spawn failure logs a warning and returns
  `None`; the only blocking `child.wait()` is on a short-lived spawner
  thread, never a supervised service.
- `src/niri.rs:1821` — the single in-compositor "restart" is the Xwayland
  satellite re-setup on a config-path change ("Try to start, or restart in
  case the user corrected the path or something"), not a crash budget.
- `resources/niri.service:3,13-14` — `BindsTo=graphical-session.target`,
  `Type=notify`, `ExecStart=niri --session`: lifetime and restart policy
  come from the service manager.
- `resources/niri-session:27-50` — the session script guards against a
  duplicate session (`systemctl --user -q is-active niri.service`),
  `reset-failed`, `import-environment`, then starts `niri.service`; shutdown
  via `niri-shutdown.target`.

Takeaway for our slice: niri's shape is the *deferred* production shape
(ADR 0003 explicitly defers the systemd user unit to 004/006). For slice 1
we do the opposite — compositor-owned supervision with a finite budget —
because the restart-budget tests must run without a service manager.

### GNOME session shutdown semantics (behavior level)

From GNOME documentation/manpages (web): the session is defined by
required components (`RequiredComponents` in
`/usr/share/gnome-session/sessions/gnome.session`); `gnome-session`
restarts required components that fail during the Initialization phase
(e.g. `gnome-settings-daemon(1)`: "gnome-session will restart" it), orders
startup/shutdown by autostart phase (`X-GNOME-Autostart-Phase`), and ends a
session through the `org.gnome.SessionManager` `EndSession` dialog/phase
protocol where clients inhibit, then the manager terminates in order.
Unconditional respawn is reserved for *required* components; ordinary apps
use XSMP restart styles (IfRunning/Session/Immediately). Our
`RestartPolicy.max_attempts` + `RecoveryAction` mirror that split: bounded
respawn for the shell, plus an overlay that keeps input alive instead of
tearing the session down.

## Implementation (`crates/compositor/src/supervise.rs`, std only)

- `Clock { now_ms() }` with `SystemClock` (wall clock, saturating u64) and
  `ManualClock` (`Cell<u64>`, `advance`/`set`) for sleep-free tests.
- `RestartPolicy { max_attempts, base_backoff_ms, max_backoff_ms }` with pure
  `next_delay(attempt) = min(base * 2^attempt, max)` (saturating, capped;
  `new` clamps `max >= base`).
- `Supervisor`: `spawn(&mut Command)` (explicit start, replaces live child);
  `poll(&mut dyn FnMut() -> Command, now_ms)` via `try_wait` — reaps exits,
  arms backoff, respawns when due; reports `BudgetExhausted` instead of
  spinning; spawn failures arm backoff and report `SpawnFailed`. Initial
  spawn is budget-free; total spawns ≤ `1 + max_attempts`. `exhaust()` is a
  pure budget report. Kill-on-drop (`kill` + reaping `wait`, best-effort).
- `RecoveryAction { ListWindows, RelaunchShell, ShowOverlay }` with doc
  comments mapping each to ADR 0003's safe-without-shell set (compositor
  state snapshot, supervisor restart path, existing input + GLES overlay).
- 6 in-module tests: backoff sequence, cap, overflow saturation, ManualClock
  budget exhaustion (no sleeps), live `/bin/true` exit, live `/bin/false`
  2-attempt exhaustion (fake clock jumps; asserts < 5 s).

## Verification

- `cargo fmt -p rwd-compositor` — clean.
- `cargo clippy -p rwd-compositor --all-targets -- -D warnings` — clean.
- `cargo test -p rwd-compositor supervise --lib` — 6 passed.
- `cargo test -p rwd-compositor` — 14 lib + handshake (1) + state_model (2),
  all green.
- `git status`: only own file touched (`supervise.rs` new content,
  `stream4-notes.md` this file); Cargo.lock/Cargo.toml/lib.rs/state.rs
  modifications belong to other streams — not touched.
