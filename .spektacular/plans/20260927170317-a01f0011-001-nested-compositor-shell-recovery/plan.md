---
created_date: "2026-09-27"
document_status: draft
---

# Plan — nested compositor and shell recovery

**Status:** Draft plan; implementation not started  
**Spec:** `001-nested-compositor-shell-recovery`

## Phase 0 — establish build contract

- Confirm spec 000 nested-backend findings and record candidate Smithay/protocol versions.
- Resolve nested-session launch/exit behavior and safe handling of host display variables.
- Define state model, revision semantics, opaque window IDs, control framing/limits, token invariants, restart backoff/budget, and recovery UI actions.
- Create ADRs for backend/protocol pin and prototype trust assumptions. Explicitly label any same-UID limitation as development-only.

**Gate:** APIs and lifecycle state transitions are reviewed before compositor UI implementation.

## Phase 1 — compositor minimum

- Create Cargo workspace/crates for compositor, shell-control schema, test clients/harness, and docs.
- Start nested Smithay display/event loop and handle client/output/seat lifecycle.
- Map GTK test app and terminal as floating surfaces; implement focus, move/resize, and one workspace.
- Add compositor-owned revisioned state and safe diagnostics.

**Evidence:** repeatable app mapping/focus journey; no shell dependency in critical input/frame callbacks.

## Phase 2 — control API and shell host

- Implement bounded local IPC, major/minor negotiation, request IDs, snapshot, ordered change subscription, revision-gap recovery, and typed command result.
- Build separate shell Wayland client with Activities trigger and current window/workspace list.
- Implement interaction token issuance/validation and compositor-side activation policy.
- Keep pointer movement, resize, and frame production local to compositor.

**Evidence:** shell presents snapshot state; malformed, stale, incompatible, and oversized control requests are rejected.

## Phase 3 — failure recovery

- Supervise shell process with finite restart attempts and capped backoff.
- Add recovery affordance independent of shell host.
- Inject process kill, disconnect, hang, crash loop, malformed IPC, and revision gap.
- Confirm app connections and input remain alive and state is rebuilt from a complete snapshot.

**Evidence:** A1–A7 are demonstrated with privacy-safe artifacts; repeated failure does not spin or block compositor.

## Exit/review gate

Review the complete spec, protocol schema, threat assumptions, trace/log output, and artifacts. Do not proceed to hardware daily-driver claims until identity questions are resolved in spec 004.
