# Stream 3 notes — failure-injection recovery harness (`tests/recovery.rs`)

## What was built

`crates/compositor/tests/recovery.rs`: 7 integration tests proving the
recovery contract end to end against real crate APIs only
(`control::ControlServer`/`Session`, `state::StateModel`,
`supervise::Supervisor`/`ManualClock`). No source file was changed.

## Reference basis (no new bodies inspected)

Supervision split and restart-budget shape follow the existing survey in
`.spektacular/work/wave2/stream4-notes.md`: niri delegates supervision to
systemd and deliberately orphans children (no in-compositor reap/restart),
while slice 1 does the opposite — compositor-owned `Supervisor` with a
finite budget — because restart-budget tests must run without a service
manager. No new reference bodies were inspected, so the survey is cited
as-is; all harness code is original.

## Evidence (001 spec A3-A6 + ADR 0003)

- Malformed frame over a real socketpair/listener: typed `MalformedFrame`
  error, same session then serves `ToggleOverview` (server stayed alive).
- Oversize prefix (> 1 MiB cap): typed `Oversize` decode error +
  `OversizeFrame` wire error; a fresh client then handshakes cleanly.
- Stale major `Hello`: typed `IncompatibleVersion` refusal; a conforming
  client then handshakes cleanly.
- Revision gap: `changes_since` forced to `Err` by overflowing
  `MAX_CHANGE_LOG`; `emit_deltas` returns `Emitted::Snapshot` and the wire
  carries a full `Snapshot` at the current revision (never `Changes`),
  with a window-id set equal to the model.
- Shell death: `sleep` child spawned under `Supervisor`, replaced via the
  supervisor kill path, then a 2-attempt budget exhausts to
  `BudgetExhausted` on `ManualClock` (wall-clock capped at 10 s).
- Disconnect/reconnect: dropped client stream, fresh handshake, full
  `Snapshot` re-receipt with revision + window set equal (no
  geometry/focus assumptions).
- Stall: unread socketpair peer; 500 multi-KB snapshots deterministically
  hit `WouldBlock` (backpressure, never a block); the stalled connection
  is dropped and a fresh handshake plus `TestCompositor::pump` complete.

## Verification

- `cargo fmt -p rwd-compositor` — clean.
- `cargo clippy -p rwd-compositor --all-targets -- -D warnings` — clean.
- `cargo test -p rwd-compositor --test recovery` — 7 passed.
- `cargo test -p rwd-compositor` — 41 passed (22 lib + 9 + 1 + 7 + 2),
  zero failures.
- `git status`: only `crates/compositor/tests/recovery.rs` (new) and this
  notes file; Cargo.lock untouched, nothing committed.
- Environment note: the run hit a transient root-disk ENOSPC (other
  sessions' multi-GB `/tmp` data, not this stream's); verification was
  retried once space eased and passed. No foreign files were touched.
