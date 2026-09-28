# Stream 1 notes — compositor-side control channel

Owner: control-channel. Owns `crates/compositor/src/control.rs`,
`crates/compositor/tests/control.rs`, and this file only.

Spec: 001 "Initial control API" (R4/R6); ADR: `docs/adr/0002-shell-control-ipc-envelope.md`.
Protocol types and frame codec come from `rwd-shell-control` (Wave 2
stream 1) and are not redefined here.

## Reference (niri survey, patterns only — original code)

Read `.spektacular/work/wave2/stream1-notes.md` (no re-clone needed):
niri-ipc pins exact releases and discovers versions via
`Request::Version`, frames JSON lines over `$NIRI_SOCKET`, and always
delivers full state up-front on the event stream. We differ per ADR 0002:
binary postcard body with u32-LE prefix, explicit
`ProtocolVersion{major,minor}` handshake with hard stale-major reject, and
a 1 MiB cap checked before decoding. cosmic-comp's full-state-on-bind
confirms snapshot-on-connect. All implementation here is original.

## Design (control.rs)

- `ControlServer::new(UnixListener)` (already-bound; try-accept) ->
  `ControlConn` (nonblocking, internal read buffer, `read_frame` /
  `write_frame` over the schema codec).
- R6: nothing blocks; not-ready surfaces as `ControlError::WouldBlock`.
  Write backpressure means drop (a partial frame may be on the wire),
  matching ADR 0002 "slow shell is disconnected".
- `Session::handshake[_with]`: Hello first, reply our version, then a full
  snapshot. Stale major -> typed `IncompatibleVersion` + refuse. Default
  token hook is `deny_all_tokens` (fail-closed; ADR 0002 policy stays
  injectable, never protocol behavior).
- Per-call `&StateModel` (`&mut` only where commands mutate) so tests can
  drive the model between calls: `send_snapshot` (on-request path),
  `emit_deltas` (incremental `Changes`, gap -> fresh snapshot), duplicate
  mid-session `Hello` -> resync, shell-sent compositor-direction messages ->
  typed `UnknownCommand`, decode failures -> mapped typed errors with
  oversize/stale-version also closing the session.

## Verify (package-scoped only)

- `cargo fmt -p rwd-compositor`
- `cargo clippy -p rwd-compositor --all-targets -- -D warnings`
- `cargo test -p rwd-compositor` (rustc 1.98.1)
