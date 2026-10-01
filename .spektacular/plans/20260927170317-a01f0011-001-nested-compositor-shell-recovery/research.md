---
created_date: "2026-09-27"
document_status: final
closed_date: "2026-09-28"
---

# Research: 20260927170317-a01f0011-001-nested-compositor-shell-recovery

## Alternatives considered and rejected

### Option A: Headless-only sufficiency

Treat the headless `TestCompositor` plus socket/control unit tests as enough for 001.

**Rejected**: `crates/compositor/src/lib.rs:1-7` and `:133-177` state no event loop, backend, seat routing, or shell channel; this cannot satisfy R1/R2/A1/A2/A7 live nested app mapping and input.

### Option B: Full anvil machinery now

Pull in full anvil `Space`, damage tracking, dmabuf feedback, and XWayland for slice 1.

**Rejected**: explicitly deferred by the 001 spec constraints and ADR 0001; scope creep that would bury the control/recovery contract under unrelated rendering work.

### Option C: Separate recovery client now

Build a second supervised recovery client instead of the compositor-owned overlay.

**Rejected**: ADR 0003 defers a second supervised process until the control contract hardens; the overlay rides the existing input/GLES path.

### Option D: Generated schema codec

Replace postcard framing with protobuf/flatbuffers.

**Rejected**: ADR 0002 already selects a compact framed local protocol for two parties; the schema crate implements u32-LE length plus postcard with a 1 MiB cap and typed decode errors (`crates/shell-control-schema/src/lib.rs:8-24,257-290`).

### Option E: systemd supervision for slice 1

Supervise the shell through a systemd user unit instead of compositor-spawned child management.

**Rejected**: ADR 0003 defers systemd to 004/006 production work; it couples the nested prototype to service-manager setup and complicates restart-budget tests.

## Chosen approach — evidence

- Start from pinned smallvil/minimal.rs nested winit shape, not full anvil: prior 001 research records `smallvil/` plus `examples/minimal.rs` as slice template and full anvil Space/damage/dmabuf/xwayland as deferred.
- Keep Smithay 0.7.0 pin set and GLES-over-EGL nested backend: ADR 0001 `docs/adr/0001-nested-backend-smithay-pin.md:19-27`; knowledge `repo:decisions/smithay-070-for-nested-slice.md`; pins in root `Cargo.toml` and `Cargo.lock`.
- Implement missing runtime seam: winit/calloop loop, socket accept, output/seat/input, xdg-toplevel mapping/focus/move/resize/workspace, layer-shell server for panel, StateModel wiring, token mint/one-use store, supervision loop, overlay rendering/input, nested launch script.
- Preserve existing contracts: state model `crates/compositor/src/state.rs:1-18`; control handshake/snapshot/delta `crates/compositor/src/control.rs:283-392`; shell client snapshot/gap handling `crates/shell-host/src/control.rs:278-360`; supervision budget/backoff `crates/compositor/src/supervise.rs:77-116,210-251`; overlay logic `crates/compositor/src/overlay.rs:29-76`.
- Keep same-UID prototype assumption development-only: 001 spec control-API section and ADR 0002 consequences; production identity remains 004 gate.

## Files examined

- `docs/roadmap.md:31-41` — 000/001 sequence and draft-plan rule.
- `docs/roadmap.md:63-64` — gate 1 requires recorded provisional backend/versions/event-loop/control/recovery scope.
- `docs/architecture.md:15-21` — compositor authority, shell client boundary, nonblocking critical paths.
- `docs/architecture.md:25` — nested developer preview tier and nonclaims.
- `docs/test-strategy.md:38-41` — 100-run shell-kill nested recovery journey and artifact rule.
- `docs/adr/0001-nested-backend-smithay-pin.md:19-46` — Smithay pin, GLES-only consequence, provisional status.
- `docs/adr/0002-shell-control-ipc-envelope.md:19-59` — framed IPC, backpressure, resnapshot, token policy, deferred codec/fd choices now resolved in code.
- `docs/adr/0003-nested-supervision-and-recovery.md:19-49` — child supervision, env hygiene, overlay-first recovery.
- `crates/compositor/src/lib.rs:1-7,100-110,133-177` — headless Display helper only; seat exists but no input routing; no loop/backend.
- `crates/compositor/src/state.rs:22-30,105-142,322-363` — caps, model API, TokenPolicy pure validation; no token table/store wiring.
- `crates/compositor/src/control.rs:1-30,203-210,283-392,406-470,546-582` — nonblocking IPC, fail-closed default, handshake/snapshot/deltas, command application; no runtime accept loop.
- `crates/compositor/src/supervise.rs:77-116,162-251` — restart budget/backoff and poll supervision; no compositor-loop integration.
- `crates/compositor/src/overlay.rs:1-16,29-76` — pure overlay state/key mapping; no rendering/input binding.
- `crates/shell-control-schema/src/lib.rs:8-24,35-63,222-242,257-290` — framing/version/token-redaction/decode errors.
- `crates/shell-host/src/lib.rs:1-16` — model/panel/control halves awaiting join-up.
- `crates/shell-host/src/control.rs:1-30,154-164,242-360` — shell handshake/snapshot/gap/activation-command client.
- `crates/shell-host/src/model.rs:41-55,128-154` — shell view model and schema adapter seam.
- `crates/shell-host/src/panel.rs:1-25,204-247` — layer-shell client structure; live runtime explicitly deferred pending compositor layer-shell.
- `crates/shell-host/src/main.rs:1-23` — supervised-child entry point; live runtime deferred.
- `crates/compositor/tests/handshake.rs:1-3` — headless socketpair xdg-toplevel mapping probe.
- `crates/compositor/tests/recovery.rs:1-26` — control/state/supervision fault-injection harness scope.
- `crates/compositor/tests/control.rs:1-16` and `crates/compositor/tests/state_model.rs:1-12` — socket-level control and model sync tests.
- Root `Cargo.toml` and `Cargo.lock` — workspace pins for smithay/calloop/protocols; smithay currently only `wayland_frontend`, no `backend_winit`.
- `.github/workflows/ci.yml` — fmt, clippy `-D warnings`, workspace tests.

## External references

- Smithay upstream tag `v0.7.0`, as recorded in ADR 0001/prior 001 research: reference for smallvil/anvil shapes; why it mattered: template scope and deferred anvil machinery.
- `wayland-protocols` 0.32.13 vendored XML, as recorded in prior 001 research protocol matrix: why it mattered: xdg-shell v7/viewporter v1/presentation v2/dmabuf v6/activation v1 baseline.
- Mesa EGL llvmpipe behavior, as recorded in `repo:gotchas/winit-backend-requires-egl.md`: why it mattered: nested preview needs working EGL; windowed WSI needs host session.

## Prior plans / specs consulted

- 001 spec via `spektacular spec file read 20260927170317-a01f0011-001-nested-compositor-shell-recovery`: R1-R7, control API, A1-A7, technical approach, risks.
- 001 draft plan via `spektacular plan file read ... plan`: Phase 0-3 and exit gate; now stale because waves implemented parts and code moved.
- 001 draft context via `... context`: intended crates and provisional constraints; code now exists but runtime integration missing.
- 001 draft research via `... research`: pins, backend API, codec recommendation, EGL/anvil follow-ups; codec and supervision pieces since implemented.
- 001 draft test plan via `... test-plan`: unit/property, nested integration, 100-run fault injection, critical-path stall, diagnostics/privacy.
- Greeter implement completion: finished workflow, CI green; relevant only as adjacent completed scope and uncommitted store artifacts to avoid mixing into 001 commits.

## Open assumptions

- 001 draft spec plus ADRs 0001-0003 satisfy roadmap gate 1 as provisional recorded inputs. If walkthrough requires spec/ADR changes, STOP and update the owning spec/ADR before implement.
- CI/dev environment provides working EGL acceptable to Smithay winit backend. If EGL probe fails, STOP: nested runtime cannot be verified as specified.
- Windowed nested runs need a host Wayland/X session; if unavailable, verify headless/offscreen paths plus structure, and flag live WSI as manual follow-up rather than claiming A1/A2.
- Existing control/state/supervision/overlay APIs are stable enough to build runtime integration on; if integration exposes contract gaps, STOP and revise plan rather than silently changing wire behavior.

## Drafting assumptions

### Target single repo for 001 (discovery)
- **Decision**: Plan all 001 changes in `rust-wayland-desktop` only.
- **Rationale**: Registry lists one repo; code, docs, ADRs, and tests all live there.
- **Rejected**: Splitting work across repos; no other registered repo exists.

### Treat 001 draft as plannable provisional input (discovery)
- **Decision**: Proceed with planning against draft 001 spec plus ADRs 0001-0003 under roadmap gate 1.
- **Rationale**: Gate 1 requires recorded provisional inputs, not final production decisions; blocking changes must surface in walkthrough.
- **Rejected**: Halting plan work solely because spec/ADRs are draft; that would stall the roadmap-ordered next slice.

### Runtime-first plan shape (discovery)
- **Decision**: Recommend integrating existing control/state/supervision/overlay pieces into a real nested runtime before adding new protocol features.
- **Rationale**: Code already implements contracts but lacks backend loop, mapping, input, layer-shell, token store, supervision wiring, and launch script.
- **Rejected**: Rewriting contracts first; current tests constrain them.

### Chosen runtime-integration direction (architecture)
- **Decision**: Integrate around existing contracts using smallvil/minimal.rs nested shape; defer full anvil machinery and production concerns.
- **Rationale**: Contracts and fault harnesses already exist; only live nested runtime can prove mapping/input/recovery acceptance.
- **Rejected**: Headless-only sufficiency; full-anvil-now scope expansion.

### No applicable conventions (architecture)
- **Decision**: Record that no always-applied repo conventions bear on 001.
- **Rationale**: Knowledge load returned zero convention entries; spec/ADRs/code contracts govern instead.
- **Rejected**: Padding the plan with generic conventions.

### Token store boundary shape (data_structures)
- **Decision**: Add a `TokenStore` issue/consume interface around pure `TokenPolicy`.
- **Rationale**: One-use removal and seat/app binding need stateful ownership; policy stays pure/testable.
- **Rejected**: Stuffing storage into policy or protocol messages.

### Real-client runs environment-gated (testing_approach)
- **Decision**: Automate A1/A2 with protocol-level clients; require real GTK/terminal binaries only where present, else manual test-plan coverage.
- **Rationale**: Protocol journeys prove mapping/input deterministically; real-client availability is environmental, not contractual.
- **Rejected**: Claiming full A1/A2 from headless tests alone; also rejecting automation because real clients may be absent.

### Seven-task split with live human verification (tasks)
- **Decision**: Seven tasks: five agent integration tasks, one agent docs task, one human live-session task.
- **Rationale**: Mixed agent/person verification must split per workflow rules; live visual proof cannot be agent-claimed.
- **Rejected**: Fewer coarse tasks that would hide the runtime seams; a human task without an agent docs prerequisite.

## Rehydration cues

- Reread 001 spec via `spektacular spec file read 20260927170317-a01f0011-001-nested-compositor-shell-recovery`.
- Reread the plan's `plan.md`, `context.md`, `research.md` via `spektacular plan file read <name> <doc>` once committed.
- Reinspect `crates/compositor/src/lib.rs`, `crates/shell-host/src/panel.rs`, root `Cargo.toml`, and `.github/workflows/ci.yml`.
- Resume plan with the CLI-reported current step; never invent `goto` targets.
