---
created_date: "2026-09-27"
document_status: draft
---

# Nested compositor and shell recovery

**Status:** Proposed; Gate #1 research complete 2026-09-27 (ADRs 0002–0003 await plan Phase 0 review)  
**Parent:** [Program architecture](../../docs/architecture.md)  
**Scope:** First implementation spek; nested developer preview only.

## Overview

Deliver the first end-to-end vertical slice of Rust Wayland Desktop: a nested Smithay compositor maps ordinary application clients, while a separate supervised shell host provides an Activities trigger and a window list. If the shell host crashes, application Wayland connections remain alive; the compositor exposes a minimal recovery affordance, restarts the shell with bounded backoff, and resynchronizes it from a complete state snapshot.

This spek establishes process boundaries and recovery behavior. It does not deliver GNOME parity, a secure hardware session, extension support, or 1.0 lock/capture security.

## Pinned foundation

- smithay 0.7.0, calloop 0.14.4, wayland-protocols 0.32.13, wayland-server 0.31.14, wayland-client 0.31.15, winit 0.30.13 — resolved and `cargo check`ed on rustc 1.94.1. Upstream reference: git tag `v0.7.0`. See [ADR 0001](../../docs/adr/0001-nested-backend-smithay-pin.md).
- Nested backend is GLES-over-EGL only (no software fallback); Mesa EGL 1.5 verified on the surfaceless platform. Windowed runs need a host Wayland/X session.
- Starting template: upstream `smallvil` shape plus `examples/minimal.rs` at the pinned revision; full anvil machinery (Space, damage tracking, dmabuf feedback, xwayland) is explicitly deferred.
- Protocol baseline: xdg-shell v7, viewporter v1, presentation v2, linux-dmabuf v6, xdg-activation v1.

## Requirements

### Functional

- **R1. Nested session:** start and stop the compositor as a nested Wayland session without taking over the host display session.
- **R2. Application mapping:** map a GTK test application and a terminal as floating windows; support basic focus, move/resize, and at least one workspace.
- **R3. Shell trigger and list:** a shell-host Activities action opens a simple overview with current windows; selecting a window requests activation through compositor policy.
- **R4. Authoritative state:** compositor owns focus, window geometry, workspace, input routing, and shell commands. Shell reconnect receives a complete snapshot before ordered changes.
- **R5. Shell crash recovery:** shell host termination or disconnect does not terminate the compositor or application Wayland connections. Show a minimal recovery affordance, restart with bounded backoff and a finite attempt budget, then resynchronize.
- **R6. Critical path:** compositor input dispatch, focus, resize, and frame scheduling never synchronously wait for shell replies or shell UI work.
- **R7. Diagnostics:** record shell lifecycle, restart count, reconnect/snapshot revision, and failures without logging sensitive application content.

### Initial control API

Use a private local IPC channel with explicit API major/minor negotiation and bounded messages. Define `Hello`, `Snapshot(revision, ...)`, ordered state changes, and request-ID-bearing shell commands. A reconnect always starts from a full snapshot; revision gaps force a new snapshot. Opaque window IDs are not reused during a compositor session. Titles are treated as untrusted, length-limited text.

The compositor validates every command. Activation must use a compositor-issued interaction token bound to a seat, purpose, and freshness window; a caller-supplied raw serial is not sufficient. Prototype token policy (expiry 30 s, one-use, seat plus `app_id` binding, log-and-deny) is recorded in [ADR 0002](../../docs/adr/0002-shell-control-ipc-envelope.md). Same-UID identity stays an explicitly development-only assumption; production identity is a 004 gate and must be resolved before treating the channel as a production security boundary.

### Non-functional

- Shell restart policy is bounded, observable, and does not create a crash loop.
- State recovery is deterministic: the shell renders from a snapshot rather than replaying assumptions from before the crash.
- Configuration and state needed for restart are local and do not require network access.
- Nested preview behavior is clearly labeled as development-only; it makes no daily-driver lock/security claim.

## Constraints

- Compositor is Rust and built on Smithay; use Smithay/calloop's event model unless measured evidence supports a change.
- Shell is a separate Wayland client. No UI logic or extension code runs inside the compositor.
- Start with floating windows and one workspace; tiling, multi-output hardware policy, XWayland, portals, notifications, and public extension APIs are out of scope here.
- Do not treat Wayland observation protocols as authorization for privileged window actions.
- The full program's accessibility and security gates remain mandatory for later release speks; this slice must not lock in a toolkit that prevents those journeys.

## Acceptance criteria

- **A1. Nested startup:** start the compositor and shell in a nested session; launch a GTK test client and terminal; verify both map and can receive input.
- **A2. Window flow:** open the overview, see both windows, select each, and verify the compositor applies focus without a shell round-trip on pointer motion or resize.
- **A3. Crash isolation:** with three application clients open, terminate the shell host in 100 repeated nested runs. Every run keeps the compositor responsive, all app connections alive, and application surfaces usable.
- **A4. Recovery:** shell restart is bounded by configured backoff/budget; on reconnect it receives a full snapshot and shows the current windows/workspace without unexpected geometry or focus changes.
- **A5. Failure containment:** repeated shell failure exhausts the finite retry budget, leaves the recovery affordance available, and does not spin or block compositor input.
- **A6. Protocol correctness:** reject incompatible API major versions, malformed/oversized messages, stale activation tokens, and revision gaps; a revision gap triggers a fresh snapshot.
- **A7. Reproducibility:** provide one documented developer command or script to start the nested session and repeat the crash/reconnect journey; retain privacy-safe logs on failure.

## Technical approach

1. Prototype the pinned Smithay nested backend (smallvil shape) and map two ordinary clients.
2. Define compositor-owned state model and initial control schema separately from rendering; IPC envelope per [ADR 0002](../../docs/adr/0002-shell-control-ipc-envelope.md), supervision and recovery per [ADR 0003](../../docs/adr/0003-nested-supervision-and-recovery.md).
3. Add a separate shell host with a deliberately small UI and an Activities trigger.
4. Add lifecycle supervision and a recovery surface/affordance; do not let a shell crash alter app surface ownership.
5. Add integration harness and failure injection for shell kill, malformed IPC, shell stall, and reconnect revision gap.
6. Record toolkit/accessibility and trusted-client identity findings for follow-on speks; do not silently decide these from prototype convenience.

## Risks and decisions

- **Same-UID identity:** Unix peer credentials alone may not distinguish the trusted shell from other processes belonging to the user. State the prototype threat model and choose a production identity/capability design before exposing privileged APIs outside the nested developer session.
- **Recovery UI:** define its safe interaction set and make it independent of the failed shell process.
- **Toolkit lock-in:** demonstrate keyboard and AT-SPI behavior before selecting GTK or a Rust-native toolkit for the long-lived shell.
- **Scope creep:** any hardware, portal, lock/auth, XWayland, extension, or parity work becomes a follow-on spek with explicit dependencies.
