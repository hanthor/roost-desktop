# ADR 0002: Shell control IPC envelope and activation-token policy

- Status: Decided (implemented 001 T2–T5; production credential binding stays a 004 gate)
- Date: 2026-09-27
- Owner: project (unassigned)
- Dependent speks: 001 (control API + shell host); downstream 002/003
  consume the state contract.

## Context

The architecture gives the shell host a versioned control API on a
separate local connection, with constraints: no shell round-trip on
critical paths, snapshot-on-reconnect, revision-gap resnapshot, typed
observable errors, untrusted titles. The 001 research questions asked
whether IPC uses a generated schema or a compact framed protocol
(frames, backpressure, reconnect, fd-passing) and how
compositor-issued activation tokens behave versus Wayland serials.

## Decision

- Transport: dedicated Unix socket (not the Wayland socket). Framed
  protocol: u32 length prefix + postcard body (codec chosen in
  implementation; schema version 0.2 carries per-window tokens).
- Limits: 1 MiB max frame; oversized/malformed/stale-version frames get
  a typed error and are dropped.
- Backpressure: bounded server-side queue; a slow shell is
  disconnected, never blocks compositor input/frame paths.
- Reconnect means full snapshot plus revision resync; live-connection
  revision gaps trigger resnapshot.
- fd-passing deferred (no current need; viable later via rustix
  SCM_RIGHTS, already in the smithay tree).
- Activation tokens: compositor mints 32-char random Bearer [REDACTED] via
  `TokenStore` with purpose/seat/app_id attached. Policy: 30 s expiry,
  one-use (remove after first successful activation), seat binding
  plus `app_id` match required, log-and-deny on mismatch. Binding to
  `XdgActivationState::create_external_token` is deferred to the 004
  production-credential decision; the wire already carries opaque
  token strings, so the mint can change without a schema bump.

## Alternatives considered

- Generated schema (e.g. protobuf/flatbuffers): heavier toolchain for
  a local two-party protocol with a hand-rolled snapshot model;
  rejected for slice 1, revisit if a third party joins the protocol.
- Incremental catch-up across disconnects: more state to get wrong
  during recovery work; snapshot-on-reconnect is simpler and matches
  the recovery evidence rule. Rejected.
- Treating Wayland serials as activation proof: serials are
  per-display u32 event counters, meaningless across processes;
  tokens are unguessable 32-char bearer strings (`rand`, 0.7.0
  `wayland::xdg_activation`). Serials rejected as bearer credential.

## Consequences

- Plan Phase 2 implements framing/limits/negotiation/snapshot exactly
  as above; malformed, stale, incompatible, and oversized requests must
  be rejected in evidence.
- Token retention (`remove_token` / `retain_tokens`) is compositor
  policy code to write and test — one-use is not protocol behavior.
- Production credential binding stays a 004 release-gate decision;
  this policy is development-grade.

## Evidence

- smithay-0.7.0 `wayland::xdg_activation` source (token format,
  `XdgActivationTokenData`, `create_external_token`,
  `XdgActivationHandler`); xdg-activation-v1 XML v1 in
  wayland-protocols 0.32.13.
- 001 research (plan store): sections 3–4, written 2026-09-27.
- 001 implementation (T2–T5): postcard framing with 1 MiB cap, typed
  errors, nonblocking hub sessions, snapshot-on-connect plus gap
  resnapshot, and the one-use `TokenStore` around the pure policy —
  proven by the control suites and the 100-run fault harness with
  replay/cross-window/seat-mismatch denials.
