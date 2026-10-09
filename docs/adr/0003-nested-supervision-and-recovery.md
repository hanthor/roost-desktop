# ADR 0003: Nested supervision and recovery affordance

- Status: Decided (implemented 001 T5; production supervision/identity stays a 004 gate)
- Date: 2026-09-27
- Owner: project (unassigned)
- Dependent speks: 001 (failure recovery); 004 owns production
  supervision/lock identity.

## Context

The architecture requires a supervised shell host and a minimal
recovery affordance available when the shell is absent, with apps
staying connected through shell restart. The 001 research questions
asked who supervises the shell (systemd user service vs compositor
launcher), nested shutdown/restart semantics, and which recovery
actions stay safe without the normal shell.

## Decision

- Nested preview: the compositor spawns the shell as a child process,
  owns a finite restart budget with capped backoff, and kills the child
  on compositor exit. A systemd user unit is deferred to production
  work (004/006) and explicitly out of slice 1.
- Env hygiene for nested-session safety: unique socket name
  (e.g. `tuna-nested-<pid>`), `WAYLAND_DISPLAY` set for the child only,
  parent host environment never mutated; nested window carries an
  identifying title; host grab/ungrab escape key documented.
- Recovery affordance for slice 1: compositor-owned emergency overlay
  on the existing input + GLES path (window list, shell relaunch,
  focus/input kept alive). A separate recovery client is deferred —
  one less supervised process while the control contract is
  provisional.

## Alternatives considered

- systemd user service for slice 1: correct production shape but
  couples the nested prototype to service-manager setup and complicates
  restart-budget tests; deferred, not rejected.
- Separate minimal recovery client: doubles supervised-process
  machinery before the control contract is stable; deferred until the
  contract hardens or the overlay proves insufficient.

## Consequences

- Plan Phase 3 evidence (kill/disconnect/hang/crash-loop/malformed-IPC/
  revision-gap injection) runs against compositor-owned supervision;
  repeated failure must not spin or block the compositor.
- Prototype trust assumptions must label same-UID limits as
  development-only; production supervision/identity is a 004 gate.

## Evidence

- 001 research (plan store): sections 5–6, written 2026-09-27.
- Architecture: recovery-and-lock section of `docs/architecture.md`.
- 001 implementation (T5): `ShellDriver` (per-tick poll, session-only
  env, finite budget with capped backoff, kill on exit) plus the
  compositor-owned overlay (model-served list, relaunch, input shield)
  wired into the nested loop; proven by the 100-run fault harness
  (kill/disconnect/stall/crash-loop/malformed/gap) with redacted
  per-run artifacts, and reproducible via `scripts/tuna-nested`
  (`docs/nested-session.md`).
