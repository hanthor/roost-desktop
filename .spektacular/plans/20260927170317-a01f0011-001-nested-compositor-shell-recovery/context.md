---
created_date: "2026-09-27"
document_status: final
closed_date: "2026-09-28"
---

# Context: 20260927170317-a01f0011-001-nested-compositor-shell-recovery

## Current State Analysis

The 001 contracts already exist and are tested, but no nested runtime joins them. `crates/compositor/src/lib.rs:133-177` is a manually-pumped headless `Display` with protocol states and a seat global but no input routing, event loop, or backend. `crates/compositor/src/state.rs:105-142` owns revisions, generational ids, snapshots, and gaps; `crates/compositor/src/state.rs:322-363` validates tokens purely with no issuance or one-use store. `crates/compositor/src/control.rs:289-392` serves handshake, snapshot, and deltas over nonblocking sockets, and `crates/compositor/src/supervise.rs:172-251` plus `crates/compositor/src/overlay.rs:34-76` implement restart budgets and overlay logic with no live loop, rendering, or input binding. `crates/shell-host/src/control.rs:155-164,248-360` and `crates/shell-host/src/model.rs:128-154` implement the shell side of the protocol, while `crates/shell-host/src/panel.rs:204-234` and `crates/shell-host/src/main.rs:15-23` explicitly defer live runtime until the compositor serves layer-shell. The workspace pins Smithay 0.7.0 with only `wayland_frontend` (`Cargo.toml:16`); the nested backend feature is not yet enabled.

## Per-Task Technical Notes

### Task: Nested runtime loop and launch

- `Cargo.toml:16` — extend the smithay dependency with the already-decided nested backend features alongside `wayland_frontend`.
- `crates/compositor/Cargo.toml:8-14` — enable the backend features and keep schema plus test-only client deps separated.
- `crates/compositor/src/runtime.rs` (new) — winit/calloop session setup, `ListeningSocketSource` accept loop, output creation, per-tick `dispatch_all_clients`/`flush_clients`, shutdown ordering.
- `crates/compositor/src/main.rs` (new) — nested binary: unique socket name, child-only `WAYLAND_DISPLAY`, identifying window title, signal-safe exit.
- `crates/compositor/src/lib.rs:133-177` — keep `TestCompositor` for deterministic headless tests; share protocol-state construction with the runtime.

**Complexity**: High
**Token estimate**: ~40k
**Agent strategy**: Parallel analysis, sequential integration

### Task: Window mapping, input, and token store

- `crates/compositor/src/lib.rs:62-80` — wire `new_toplevel` into window registration instead of discarding surfaces; keep commit/buffer observation.
- `crates/compositor/src/windows.rs` (new) — floating geometry, focus, move/resize, single workspace, seat routing; mutations flow through `StateModel`.
- `crates/compositor/src/state.rs:322-363` — keep `TokenPolicy` pure; add a `TokenStore` owner for mint, one-use consume, seat and app binding.
- `crates/compositor/src/control.rs:546-582` — inject the live token-store validator into `apply_command` instead of the fail-closed default.

**Complexity**: High
**Token estimate**: ~40k
**Agent strategy**: Parallel analysis, sequential integration

### Task: Live shell overview and activation wiring

- `crates/shell-host/src/control.rs:155-164,248-280,286-360` — connect the client to the live control socket; handshake, snapshot, delta, gap, and activation-result flows drive the model.
- `crates/shell-host/src/model.rs:128-154` — keep the snapshot-view seam; map live focus and workspace truth into selection and overview state.
- `crates/compositor/src/control.rs:289-345,364-392` — serve the live shell from the existing handshake, snapshot, and delta paths without wire changes.

**Complexity**: Medium
**Token estimate**: ~25k
**Agent strategy**: 2-3 parallel agents for independent changes

### Task: Layer-shell panel bring-up

- `crates/compositor/src/layer.rs` (new) — advertise and manage the layer-shell global: top-anchored panel surface, exclusive zone, configure ack, close handling.
- `crates/shell-host/src/panel.rs:204-234` — run the existing panel setup against the live compositor; no fallback surface roles.
- `crates/shell-host/src/main.rs:15-23` — lift the deferred-runtime guard once the compositor serves layer-shell; keep the clear error for nonconforming compositors.

**Complexity**: Medium
**Token estimate**: ~20k
**Agent strategy**: 2-3 parallel agents for independent changes

### Task: Supervision binding and 100-run recovery harness

- `crates/compositor/src/supervise.rs:172-251` — drive `Supervisor::poll` from the runtime tick; spawn the shell binary as child, enforce budget and backoff, kill on exit.
- `crates/compositor/src/overlay.rs:34-76` — bind overlay visibility, selection, and relaunch actions to live input and the render path.
- `crates/compositor/tests/recovery.rs:1-50` — extend the harness to live kill, disconnect, stall, crash-loop, malformed IPC, and revision-gap runs with per-run revision and connection assertions.
- `crates/compositor/src/main.rs` (new in T1) — own shell spawn environment and restart wiring.

**Complexity**: High
**Token estimate**: ~35k
**Agent strategy**: Parallel analysis, sequential integration

### Task: Launch script, docs, and redacted diagnostics

- `docs/nested-session.md` (new) — one-command launch, crash/reconnect journey, env hygiene, host grab/ungrab notes.
- `scripts/rwd-nested` (new) — deterministic launcher wrapping the compositor binary with socket naming and artifact directories.
- `docs/adr/0001-nested-backend-smithay-pin.md:38-46`, `docs/adr/0002-shell-control-ipc-envelope.md:51-59`, `docs/adr/0003-nested-supervision-and-recovery.md:44-49` — flip provisional statuses only where implementation evidence hardens them.
- `crates/compositor/src/supervise.rs:130-160` — keep error and event types as the redaction-safe diagnostic vocabulary.

**Complexity**: Medium
**Token estimate**: ~15k
**Agent strategy**: Single agent, sequential execution

### Task: Live-session visual verification

- **What the person must do**: on a live host Wayland/X session, run the documented launch command, open real application clients, check panel placement and window behavior, kill the shell, and exercise the recovery overlay.
- **How the result is checked**: record environment details, observed behavior per acceptance item, and any discrepancy as follow-up work; automation results alone do not close this task.

## Testing Strategy

- T1 runtime loop: nested startup/shutdown journeys, env-hygiene assertions, frame/client dispatch progress without shell; EGL-gated, STOP on probe failure.
- T2 mapping and tokens: protocol-client mapping/input journeys, focus/move/resize/workspace cases, token issue/expiry/seat/app/replay unit and integration cases.
- T3 shell wiring: live snapshot/delta/gap journeys, overview selection, activation result correlation, negative token and version cases.
- T4 panel: layer-surface attach, exclusive zone, configure ack, close behavior; no fallback roles permitted.
- T5 recovery: 100-run kill/disconnect/stall/crash-loop harness with per-run connection and revision assertions, budget exhaustion calmness, overlay availability, redacted artifacts.
- T6 docs: launch-command reproducibility check, artifact redaction scan, ADR status updates.
- T7 human: live visual confirmation and recorded evidence; closes windowed runs automation cannot claim.

## Project References

- Spec: 001 nested compositor and shell recovery (`spektacular spec file read 20260927170317-a01f0011-001-nested-compositor-shell-recovery`), R1-R7 and A1-A7.
- ADRs: `docs/adr/0001-nested-backend-smithay-pin.md`, `docs/adr/0002-shell-control-ipc-envelope.md`, `docs/adr/0003-nested-supervision-and-recovery.md`.
- Knowledge: `repo:decisions/smithay-070-for-nested-slice.md`, `repo:learnings/smithay-070-pin-set.md`, `repo:gotchas/winit-backend-requires-egl.md`.
- Roadmap gates: `docs/roadmap.md` gate 1 provisional inputs; test strategy `docs/test-strategy.md` nested fault journeys.
- Repo root: `/home/ubuntu/dev/rust-wayland-desktop` (registry name `rust-wayland-desktop`).

## Token Management Strategy

| Tier | Token Budget | Agent Strategy |
|------|-------------|----------------|
| Low | ~10k | Single agent, sequential |
| Medium | ~25k | 2-3 parallel agents |
| High | ~50k+ | Parallel analysis, sequential integration |

Plan total is roughly 175k across six agent tasks (40 + 40 + 25 + 20 + 35 + 15). Run T1 first alone since every other task builds on the runtime; T3 and T4 can parallelize once T1/T2 land; keep T5 sequential after the live shell exists. The human T7 carries no agent token cost.

## Migration Notes

N/A — no existing users, data, or wire versions to migrate; the control schema stays frozen and new runtime modules are additive.

## Performance Considerations

Frame scheduling, input dispatch, focus, and resize must never synchronously wait on shell replies, control I/O, or supervision actions; the existing nonblocking control contract and bounded supervisor behavior are the enforcement points. The 100-run fault harness must stay time-bounded with per-run budgets rather than wall-clock sleeps, and run artifacts must stay small and redacted. No performance thresholds are asserted here; comparable measurement belongs to specs 000 and 006.
