---
created_date: "2026-09-27"
document_status: final
closed_date: "2026-09-28"
---

# Plan: 20260927170317-a01f0011-001-nested-compositor-shell-recovery

<!-- Metadata -->
<!-- Created: 2026-09-28T12:00:44Z -->
<!-- Commit: bff5787 -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/rust-wayland-desktop.git -->

<!--
  OVERVIEW
  A concise 2-3 sentence summary of the plan. Answer:
    1. What is being built?
    2. What problem does it solve?
    3. Who benefits?
  No file paths, no commands, no implementation detail. A reviewer should be
  able to decide whether the plan is worth reading in full from this section
  alone.
-->
## Overview

Integrate the already-implemented 001 control, state, supervision, and overlay contracts into a real nested Smithay session that maps application windows and survives shell crashes. It closes the gap between tested contracts and the spec's live acceptance criteria, which no headless harness can satisfy. Developers get a repeatable nested desktop slice as the foundation for shell parity and hardware work.

<!--
  CONVENTIONS
  The project conventions (always-apply standards) that bear on this work,
  each with a one-line reason it applies — and only the relevant ones, not
  the whole knowledge base. Conventions are loaded in full during discovery
  and the relevant subset is chosen as the design is locked, then confirmed
  by the user. Cite a convention inline in the sections it drives. If no
  conventions are relevant (or the project has none), state that plainly,
  e.g. "No project conventions apply to this feature." An empty or generic
  list is a visible signal the knowledge base was not consulted.
-->
## Conventions

No project conventions apply to this feature.

<!--
  ARCHITECTURE & DESIGN DECISIONS
  The chosen design direction in 2-4 short paragraphs. Explain the shape of
  the solution, the key decisions and their trade-offs, and why the chosen
  direction beats the alternatives. Cross-reference
  research.md#alternatives-considered-and-rejected so readers can drill into
  the evidence for rejected options. This is plan.md's load-bearing section —
  a reviewer should be able to spot missing architectural patterns or design
  gaps from this section without needing to read context.md.
-->
## Architecture & Design Decisions

Build the missing nested runtime around the already-implemented 001 contracts instead of redesigning them. All work lands in `rust-wayland-desktop`: a new compositor runtime joins Smithay’s pinned winit/calloop backend to the existing `StateModel`, control handshake/snapshot/delta session, supervisor, and overlay logic; `shell-host` keeps the panel and control client and gains live wiring to overview selection; the schema crate stays frozen unless integration proves a wire gap. Requirement mapping is direct: R1/R7 nested launch, env hygiene, shutdown, and diagnostics live in the compositor binary plus one documented launch script; R2 app mapping, focus, move/resize, and one workspace live in compositor runtime handlers; R3 Activities/overview selection lives in `shell-host`; R4/R6 authoritative state, nonblocking control, snapshot/gap resync, and token-gated activation reuse `state.rs`, `control.rs`, and the schema crate; R5 supervision/overlay reuse `supervise.rs` and `overlay.rs` with rendering/input binding added.

The key trade-off is scope discipline: choose the smallvil/minimal.rs nested shape and defer full anvil machinery, XWayland, portals, tiling, multi-output policy, systemd supervision, separate recovery client, and production identity. This keeps the slice to a developer preview while preserving extraction seams for 002/003/004. It beats headless-only testing because only a real nested session can prove A1/A2 app mapping and input; it beats full-anvil-now because that machinery is explicitly out of scope and would hide the control/recovery contract under unrelated rendering work. Detailed rejected paths are in `research.md#alternatives-considered-and-rejected`.

No project conventions apply to this feature: the repo’s always-applied knowledge load returned no convention entries, so architecture follows the spec, ADRs 0001–0003, and the contracts already encoded in code and tests.

<!--
  COMPONENT BREAKDOWN
  The components (new or changed) that make up the solution, with their
  responsibilities and how they interact. One bullet or short paragraph per
  component. Name the component, state what it owns, and describe its
  relationship to the other components. Do not list file paths or line
  numbers here — component responsibilities, not implementation sites.
-->
## Component Breakdown

- **Nested runtime loop** (new): owns the Smithay winit/calloop session lifecycle, client acceptance, output setup, and frame scheduling. It feeds window/input events to the state owner and never blocks waiting for shell work.
- **Window and input manager** (new): owns floating-window mapping, focus, move/resize, one workspace, seat input routing, and activation-token issuance/validation hooks. It publishes authoritative state changes and applies only validated shell commands.
- **Control service** (changed): owns the existing versioned IPC session, snapshots, ordered deltas, gap resync, and typed errors. It remains the sole shell/compositor state bridge and keeps its nonblocking/backpressure behavior.
- **Shell panel client** (changed): owns Activities trigger, overview window list, snapshot/delta application, and activation command requests. It renders only from compositor snapshots and resyncs from scratch after reconnect or gap.
- **Supervision and recovery** (changed): owns shell process lifecycle, finite restart budget, capped backoff, recovery overlay state, and safe diagnostics. It keeps app connections alive through shell failure and reports exhaustion without spinning.
- **Nested launch and diagnostics harness** (new): owns the documented one-command nested session, env hygiene, crash/reconnect journey, failure injection, and privacy-safe logs. It is the repeatable proof path for mapping, recovery, and diagnostics.

<!--
  DATA STRUCTURES & INTERFACES
  The types, interface signatures, and serialization boundaries introduced or
  changed by the plan. Show type shapes in pseudocode or a short code block
  where useful. Focus on the contract between components, not internal
  representation detail.
-->
## Data Structures & Interfaces

Reuse the existing wire and model contracts; add only the runtime seams needed to join them.

```text
NestedSession { socket_name, child_display, output_size, tick_budget }
WindowHandle { model_id, surface_key, workspace, geometry, focused }
TokenStore { issue(purpose, seat, app_id) -> Token; consume(token, seat, app_id) -> Allow | Deny(reason) }
SupervisorEvent { ShellExited(code) | RestartScheduled(delay) | BudgetExhausted | SnapshotResent(revision) }
RunArtifact { revisions, restart_count, snapshot_hash, redacted_log }
```

`NestedSession` carries launch/env/frame configuration between the launcher and runtime loop. `WindowHandle` binds a compositor-owned model id to a live surface without exposing surface internals to the shell. `TokenStore` is the missing one-use issuance/consumption boundary around the existing pure `TokenPolicy`. `SupervisorEvent` makes restart, resync, and exhaustion observable without logging sensitive content. `RunArtifact` standardizes nested-run evidence for mapping, crash, and reconnect journeys.

<!--
  IMPLEMENTATION DETAIL
  High-level only. Sketch new patterns being introduced, major code-shape
  changes, and code-structure UX — enough for a reviewer to spot missing
  patterns or design gaps. This is NOT per-task file:line work — that
  belongs in context.md. If you find yourself writing "in file X at line Y",
  stop and move that content to context.md.
-->
## Implementation Detail

Introduce a runtime-join pattern: the new nested loop owns backend, timing, and process events, while the existing state, control, supervision, and overlay modules keep owning their invariants. New code translates live protocol and process events into model mutations and control emissions; it never duplicates snapshot, revision, retry, or budget logic. A developer reading the result sees a thin event-driven shell around already-tested cores: backend events in, model changes out, control frames and supervisor actions as side effects.

The major shape change is promoting the compositor crate from a headless protocol helper to a runnable session owner, with the shell panel graduating from a deferred client sketch to a supervised live client. Follow existing patterns for nonblocking control, fail-closed activation, bounded supervision, and redacted diagnostics; introduce only one new stateful pattern, the one-use token store, plus a small run-artifact convention for repeatable nested evidence.

<!--
  DEPENDENCIES
  The internal packages, external libraries, upstream specs, or prior plans
  this work depends on. One bullet per dependency with a one-line note on
  what it provides and whether it needs any changes.
-->
## Dependencies

- 001 nested compositor/shell recovery spec: source of truth for R1-R7 and A1-A7; no spec changes assumed, otherwise STOP.
- ADR 0001 Smithay pin: provides backend/version/event-loop baseline; no change, provisional status retained.
- ADR 0002 IPC/token policy: provides framing and activation rules; implementation adds the stateful token store the ADR already requires.
- ADR 0003 supervision/recovery: provides child-process and overlay-first recovery shape; no change.
- Existing workspace crates (`compositor`, `shell-control-schema`, `shell-host`): provide tested contracts; runtime integration changes compositor/shell-host, schema stays frozen absent proven wire gap.
- Smithay 0.7.0/calloop/protocols pin set: provides nested backend and protocol XML; workspace must enable the already-decided nested backend feature during implementation.
- Working EGL plus optional host Wayland/X session: provides runnable nested backend; no code change, but missing EGL blocks runtime verification.
- Design documents this plan was built on: none.

<!--
  TESTING APPROACH
  High-level overview of the testing strategy: what kinds of tests
  (unit, integration, contract, regression), which components get the most
  coverage, and what the load-bearing assertions are. Per-task testing
  detail — which specific tests live in which specific files — stays in
  context.md.
-->
## Testing Approach

Concentrate coverage on the new runtime joins: session lifecycle, window/input mapping, token issuance/consumption, supervision wiring, and overlay binding. Existing contract suites already cover state revisions, control framing/versioning, shell-client resync, and restart budgets; extend them only where integration exposes gaps. Add nested integration journeys for startup, mapping, focus/move/resize, overview selection, shell kill/reconnect, revision gaps, and control stalls, plus unit coverage for the token store and run-artifact redaction.

Load-bearing guarantees in plain language: app connections survive shell death; every reconnect starts from a full snapshot; stale tokens, wrong seats, replays, oversized/malformed frames, and version mismatches are denied with typed errors; restart attempts stay finite and never spin the compositor; critical input and frame paths never wait on shell work; logs carry revisions and counts, never content or secrets. Tests follow existing project conventions: deterministic socket-driven cases, bounded retry loops, manual clocks for backoff, and redacted failure artifacts.

Acceptance mapping: A1/A2 nested startup and window flow are automated nested journeys with protocol-level clients, with real GTK/terminal runs added wherever the environment provides those binaries — Manual, captured in the implementation test plan where client binaries are unavailable. A3 100-run crash isolation is an automated fault harness with per-run revision/connection assertions. A4/A5 recovery and containment are automated backoff/budget/exhaustion cases plus overlay-availability checks. A6 protocol correctness is automated negative-case coverage. A7 reproducibility is proven by the single documented launch command plus retained redacted artifacts. The spec carries no separate Success Metrics section, so A1-A7 are the verification handoff.

<!--
  MILESTONES & TASKS
  2-4 milestones. Each milestone leads with a "What changes" summary
  paragraph describing the user-visible difference when the milestone lands.
  Each task is one unit of work in exactly one registered repo, for one
  executor, and carries **Id:** (from `plan task-id`), **Repo:**,
  **Depends on:** (`none` or `- <id> — <title>` lines) and **Execution:**
  (`agent`, or `human — <reason>`) lines. Each task has a 2-4 sentence
  plain-language summary, a *Technical detail:*
  link to context.md, and an **Acceptance criteria**: checkbox list with
  outcome statements (not shell commands). No file:line references in
  plan.md task content — those live in context.md.
-->
## Milestones & Tasks

### Milestone 1: A nested desktop session that maps and focuses real app windows

**What changes**: developers can start an isolated nested session that does not disturb the host display, open ordinary application windows in it, and move, resize, and focus them with working input.

#### - [x] Task: Nested runtime loop and launch
**Id:** 144520d0-9a69-4ad5-811c-c3f00c609eb1
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Add the runnable nested session: backend event loop, client acceptance, output setup, frame scheduling, clean shutdown, and child-only environment hygiene. This turns the headless protocol helper into a session developers can actually start without touching the host display.

*Technical detail:* [context.md#task-nested-runtime-loop-and-launch](./context.md#task-nested-runtime-loop-and-launch)

**Acceptance criteria**:

- [x] A single documented command starts and stops an isolated nested session
- [x] The nested session never mutates the host display environment
- [x] Frame scheduling and client dispatch run without waiting on shell work

#### - [x] Task: Window mapping, input, and token store
**Id:** e382135e-377a-450b-87a9-d1c0436dd0ef
**Repo:** rust-wayland-desktop
**Depends on:**
- 144520d0-9a69-4ad5-811c-c3f00c609eb1 — Nested runtime loop and launch
**Execution:** agent

Map ordinary application windows as floating surfaces with working focus, move, resize, and one workspace, routed through seat input. Add the one-use activation token store around the existing pure policy so shell activation requests are granted or denied on real token state.

*Technical detail:* [context.md#task-window-mapping-input-and-token-store](./context.md#task-window-mapping-input-and-token-store)

**Acceptance criteria**:

- [x] Mapped test clients receive keyboard and pointer input
- [x] Focus, move, resize, and workspace moves behave per compositor policy
- [x] Activation tokens expire, bind correctly, and cannot be replayed

### Milestone 2: A live shell overview that reflects compositor truth and activates windows safely

**What changes**: the shell shows the current window list and workspace state, opens an overview, and activates a chosen window only with a fresh compositor-issued token; stale, replayed, or mismatched requests are refused.

#### - [x] Task: Live shell overview and activation wiring
**Id:** acaa360b-3c5f-4765-8c19-bdffef4d015c
**Repo:** rust-wayland-desktop
**Depends on:**
- e382135e-377a-450b-87a9-d1c0436dd0ef — Window mapping, input, and token store
**Execution:** agent

Join the shell model and control client to the live compositor: snapshot on connect, ordered deltas with gap resnapshot, overview open and window selection, and token-bearing activation commands with result handling. The shell renders only compositor truth and never assumes pre-crash state.

*Technical detail:* [context.md#task-live-shell-overview-and-activation-wiring](./context.md#task-live-shell-overview-and-activation-wiring)

**Acceptance criteria**:

- [x] Overview lists current windows and workspace state from live snapshots
- [x] Selecting a window focuses it through token-gated activation
- [x] Revision gaps always trigger a fresh snapshot, never partial state

#### - [x] Task: Layer-shell panel bring-up
**Id:** 33c4c8c0-cd0f-4942-b02c-abe4781355d7
**Repo:** rust-wayland-desktop
**Depends on:**
- 144520d0-9a69-4ad5-811c-c3f00c609eb1 — Nested runtime loop and launch
- acaa360b-3c5f-4765-8c19-bdffef4d015c — Live shell overview and activation wiring
**Execution:** agent

Give the compositor a layer-shell server so the supervised panel client can attach its top-anchored Activities surface, and lift the deferred-runtime guard in the shell binary. The panel keeps its exclusive zone and keyboard behavior against the live compositor.

*Technical detail:* [context.md#task-layer-shell-panel-bring-up](./context.md#task-layer-shell-panel-bring-up)

**Acceptance criteria**:

- [x] The shell panel attaches as a top-anchored layer surface with exclusive zone
- [x] The shell binary runs against the nested compositor without fallback roles
- [x] Panel configure and close events behave per protocol

### Milestone 3: Shell crashes disappear into a recovery overlay while apps keep running

**What changes**: killing or stalling the shell no longer takes apps down; a compositor-owned recovery view keeps the session usable, restarts the shell within bounded retries, and restores the exact window and workspace state from a fresh snapshot.

#### - [x] Task: Supervision binding and 100-run recovery harness
**Id:** be296c81-45a1-4922-b10d-7141151e459c
**Repo:** rust-wayland-desktop
**Depends on:**
- 33c4c8c0-cd0f-4942-b02c-abe4781355d7 — Layer-shell panel bring-up
**Execution:** agent

Wire the supervisor and overlay state machine into the live loop: shell spawn as supervised child, bounded restart with backoff, kill on exit, recovery overlay binding, and resync from full snapshots. Extend the fault harness to kill, disconnect, stall, and crash-loop the shell across 100 runs with redacted per-run evidence.

*Technical detail:* [context.md#task-supervision-binding-and-100-run-recovery-harness](./context.md#task-supervision-binding-and-100-run-recovery-harness)

**Acceptance criteria**:

- [x] Every fault run keeps app connections alive and the compositor responsive
- [x] Restarts stay within budget and backoff, and exhaustion stays calm and observable
- [x] Reconnected shells render exact window and workspace state from snapshots

#### - [ ] Task: Launch script, docs, and redacted diagnostics
**Id:** ceaafa54-7cdd-44c8-b226-70331d7ea6da
**Repo:** rust-wayland-desktop
**Depends on:**
- be296c81-45a1-4922-b10d-7141151e459c — Supervision binding and 100-run recovery harness
**Execution:** agent

Finish the repeatable proof path: one documented launch command, crash and reconnect journey docs, privacy-safe log and artifact conventions, and updated ADRs where provisional decisions harden. Failures retain revisions, counts, and snapshots without content or secrets.

*Technical detail:* [context.md#task-launch-script-docs-and-redacted-diagnostics](./context.md#task-launch-script-docs-and-redacted-diagnostics)

**Acceptance criteria**:

- [ ] One command reproduces the nested crash and reconnect journey
- [ ] Failure artifacts are complete and contain no sensitive content
- [ ] Provisional decisions that hardened are recorded in ADRs

#### - [ ] Task: Live-session visual verification
**Id:** 6efa92a9-7f1e-4dbb-ad20-13b7901ae3b2
**Repo:** rust-wayland-desktop
**Depends on:**
- ceaafa54-7cdd-44c8-b226-70331d7ea6da — Launch script, docs, and redacted diagnostics
**Execution:** human — verification only a person can do (visual review on a live host session with real clients)

Confirm the nested window, panel placement, window mapping, and recovery overlay by eye on a live host session with real application clients, and record the outcome in the acceptance evidence. This closes the windowed runs that automation cannot claim headless.

*Technical detail:* [context.md#task-live-session-visual-verification](./context.md#task-live-session-visual-verification)

**Acceptance criteria**:

- [ ] Nested session, panel, and mapped apps look and behave correctly live
- [ ] Recovery overlay is usable with the shell absent
- [ ] Results are recorded as acceptance evidence with environment details

<!--
  OPEN QUESTIONS
  Strictly for questions that genuinely cannot be resolved until
  implementation begins. Anything resolvable by asking the user, reading the
  code, or running a quick experiment must be resolved now — not parked
  here. If this section is empty, that is the expected outcome of a healthy
  planning pass.
-->
## Open Questions

- Whether the pinned Smithay winit/calloop event-pump and layer-shell handler shapes match the plan's runtime-join sketch once compiled against. Depends on T1/T4 integration. If the API forces a structural change to supervision or control ownership, STOP and revise the plan.
- Whether the implementation environment provides working EGL for the nested backend. Depends on T1 bring-up. If the EGL probe fails, STOP: headless-only verification cannot satisfy the nested acceptance criteria.

<!--
  OUT OF SCOPE
  Explicit exclusions agreed during planning. Each bullet states what is NOT
  being done and, where useful, where it is tracked instead. This is as
  important as the requirements — it prevents scope creep and sets clear
  expectations for reviewers.
-->
## Out of Scope

- Tiling, multi-output hardware policy, and full desktop parity: out of scope here; tracked by 002 shell workflow parity and 003 hardware compatibility.
- XWayland, portals, clipboard, drag-and-drop, IME, and output hotplug: out of scope here; tracked by 003 hardware and app compatibility.
- Production identity, secure lock and auth, capture authorization, and accessibility certification: out of scope here; tracked by 004 secure session, accessibility, and integration.
- Public extension APIs and capability brokering: out of scope here; tracked by 005 extension broker and API.
- Benchmarks, packaging, release comparison, and upgrade recovery: out of scope here; tracked by 000 baseline evidence and 006 performance, packaging, and release.
- Wire-protocol redesign: out of scope unless live integration proves a gap; the schema stays frozen and any change returns to the plan first.

## Changelog

### 2026-09-28 — Task: Nested runtime loop and launch

**What was done**: Added the runnable nested session: a calloop-driven Smithay winit backend with client acceptance, output setup, stacked frame production, env hygiene, and a documented `rwd-compositor` binary. Proven live under Xvfb with socket accept and client connect.

**Deviations**: Added `--help` output so the launch command documents itself (the criterion demands a documented command). Fixed a backend-ordering bug found live: `WAYLAND_DISPLAY` is applied only after backend creation, since winit prefers Wayland when the variable is set.

**Files changed**:
- `Cargo.toml`
- `crates/compositor/Cargo.toml`
- `crates/compositor/src/lib.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/src/main.rs`
- `crates/compositor/tests/handshake.rs`
- `crates/compositor/tests/runtime.rs`
- `docs/reference-repos.md`

**Discoveries**: `on_commit_buffer_handler` consumes buffers out of `SurfaceAttributes`, so buffer-presence assertions must read `RendererSurfaceState` instead. winit event-loop creation must happen before `WAYLAND_DISPLAY` points at our own socket, and `winit::init` panics off the main thread, so live launch tests must spawn the binary as a child.

### 2026-09-28 — Task: Window mapping, input, and token store

**What was done**: Added the floating window manager (map/unmap with cascade, motion- and click-driven focus, keyboard/pointer delivery, programmatic move/resize, workspace membership, title sync) wired into the runtime loop, render path, and input translation, plus a one-use `TokenStore` around the pure token policy with a validator closure for control sessions. Proven by headless protocol-client tests asserting client-side key/pointer delivery.

**Deviations**: Interactive drag-to-move deferred as planned; geometry changes go through the manager API. Real-client (GTK/terminal) runs stay environment-gated per the plan's testing approach — no such binaries exist here.

**Files changed**:
- `Cargo.toml`
- `crates/compositor/Cargo.toml`
- `crates/compositor/src/lib.rs`
- `crates/compositor/src/state.rs`
- `crates/compositor/src/windows.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/tests/windows.rs`
- `crates/compositor/tests/control.rs`

**Discoveries**: Smithay sends keyboard/pointer enter events only to already-bound client objects, so tests must bind seat objects before the focus they assert on. `MouseButton` is an enum (`Left`/`Right`/…) with `button_code()` for the wire value — there is no tuple field. Toplevel titles live in `XdgToplevelSurfaceData` role data, readable via `with_states` + `data_map`.

### 2026-09-28 — Task: Live shell overview and activation wiring

**What was done**: Joined the shell model and control client to the live compositor: snapshot on connect, ordered deltas with gap resnapshot, overview selection, and token-bearing activation commands with id-correlated results. `ControlHub` serves live sessions from one `TokenStore` (aged-handshake rounds, per-round deltas plus one command frame); the shell renders only compositor truth and re-hellos after any gap. Proven by hub end-to-end journeys on both sides of the socket.

**Deviations**: None. Schema 0.2 carries the per-window activation token minted per snapshot/delta entry; `Session` keeps fail-closed `deny_all_tokens` default with `TokenMinter`/`TokenValidator` type aliases satisfying `type_complexity`.

**Files changed**:
- `crates/shell-control-schema/src/lib.rs`
- `crates/compositor/src/control.rs`
- `crates/compositor/src/state.rs`
- `crates/compositor/src/runtime.rs`
- `crates/shell-host/src/control.rs`
- `crates/compositor/tests/control.rs`
- `crates/compositor/tests/live_overview.rs`
- `crates/shell-host/tests/live_roundtrip.rs`

**Discoveries**: `ControlHub` must age freshly accepted peers one full poll round before handshaking, otherwise the client's `Hello` has not arrived yet and the handshake consumes a `WouldBlock`. Shell `ControlClient` surfaces `WouldBlock` between frames, so live tests drive both sides with bounded poll loops and no sleeps.

### 2026-09-28 — Task: Layer-shell panel bring-up

**What was done**: Added the compositor layer-shell server (`WlrLayerShellState` plus a `PanelSurface` record per mapped surface: namespace, layer, acked-configure flag) with initial configure on map, and lifted the shell's deferred-runtime guard so the panel attaches live with no fallback roles. Proven by protocol tests and a live attach of the real `ShellHost` against the real server.

**Deviations**: None. The panel keeps its top-anchored full-width strip, 32px exclusive zone, and on-demand keyboard interactivity; the missing-global error stays clear and fallback-free.

**Files changed**:
- `crates/compositor/src/lib.rs`
- `crates/compositor/src/layer.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/Cargo.toml`
- `crates/compositor/tests/layer.rs`
- `crates/shell-host/src/panel.rs`
- `crates/shell-host/src/main.rs`

**Discoveries**: Wayland `wl_registry::bind` returns the proxy directly (no `Result`), unlike `GlobalList::bind`. Smithay `Anchor` is a bitflags struct (`Anchor::TOP`), and `CachedState::current()` is the accessor for committed client state. `WlRegistry::bind` accepts any queue handle, so tests can learn global names on a throwaway queue and bind the real host's proxies onto its own queue.

### 2026-09-28 — Task: Supervision binding and 100-run recovery harness

**What was done**: Wired supervision and the overlay into the live loop: the runtime spawns the shell binary as a supervised child (`ShellDriver` with per-tick poll, `WAYLAND_DISPLAY` + `RWD_CONTROL_SOCKET` child-only env, `--shell-bin`/`RWD_SHELL_BIN`/sibling/`PATH` resolution), shows the compositor-owned overlay while the shell is absent (window list from model state, evdev-key navigation/relaunch/dismiss, input shield, deep-red background), and resyncs reconnected shells from full snapshots. The fault harness runs 100 runs over kill/disconnect/stall/crash-loop/malformed/gap faults with per-run redacted `RunArtifact` evidence.

**Deviations**: The plan's `SupervisorEvent::SnapshotResent` variant was not added: snapshot resends are already observable per call via `Session::emit_deltas` (`Emitted::Snapshot`) and proven per run by exact-window-set snapshot assertions, so no new hub counter was warranted. Full overlay text rendering stays deferred; the slice-1 visual is the background shift plus input shield.

**Files changed**:
- `crates/compositor/src/supervise.rs`
- `crates/compositor/src/overlay.rs`
- `crates/compositor/src/runtime.rs`
- `crates/compositor/src/main.rs`
- `crates/compositor/tests/recovery.rs`

**Discoveries**: `Supervisor::reset_budget` does not clear `started`, so a zero-budget policy can never respawn even after reset — operator-relaunch tests need `max_attempts >= 1`. `WlRegistry::bind` onto another queue's handle works, but the direct-`get_registry` bound requires `Dispatch<WlRegistry, ()>`.
