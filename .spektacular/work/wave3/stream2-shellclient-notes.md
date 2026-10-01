# Wave 3 Stream 2 notes — shell-side control client (`shell-host`)

Spec: 001 R3 + ADR 0002 (`docs/adr/0002-shell-control-ipc-envelope.md`);
protocol types/codec from `rwd-shell-control`
(`crates/shell-control-schema/src/lib.rs`). This stream owns
`crates/shell-host/**` only (plus this note). All implementation below
is original; GNOME/COSMIC notes in
`.spektacular/work/wave2/stream3-notes.md` were reread for behavior
reference (toggle-guard path, activation-dismisses-overview,
layer-shell `Top` band) — no code copied.

## What was built

- `crates/shell-host/src/control.rs` (new) — `ControlClient` over a
  connected `UnixStream`, set nonblocking at construction; `WouldBlock`
  always surfaces as `ControlError::Io`, never retried internally.
  - Handshake: `send_hello` + `await_hello` halves (`hello` = both);
    expects a compatible `Hello` or fails on the typed `Error`
    (`Remote`). A stale-major `Hello` never decodes — the schema
    rejects it first (`Decode(IncompatibleVersion)`); a newer minor is
    reported as typed `IncompatibleVersion`.
  - Snapshot: applied into `ShellModel` via the new
    `apply_snapshot_view(SnapshotView)`; clears `needs_snapshot`, sets
    `revision`. Shadow copies of schema `WindowInfo`/`WorkspaceInfo`
    are kept so `Changes` deltas (`Opened` restates, `Closed` removes,
    `Focused` re-singles, `Moved` retargets, `WorkspaceChanged`
    restates) rebuild the view without churning `model.rs`.
  - Resnapshot rule: `Changes.from_revision != held revision`, or a
    typed `ErrorKind::RevisionGap`, yields `Handled::Gap`, sets
    `needs_snapshot`, drops the delta. `request_snapshot()` re-sends
    `Hello` — the protocol has no snapshot-request opcode, so re-hello
    is the resync signal. Fresh/reconnected clients start revisionless
    with the flag set.
  - `send_activation(token)` targets the model's selected window
    (overview-list behavior always activates out of a selection;
    `NoActiveWindow` when empty), builds the schema `ActivateWindow`
    command, returns the request id; later `CommandResult`s correlate
    by that id. Mapping results into `select_window` + overview
    dismissal is later UI wiring, deliberately left to the caller.
  - Schema-mapping seam (commented in code): `focused -> active`,
    workspace ids narrow `u64 -> u32` saturating at `u32::MAX`,
    titles pass through (length capped at decode; rendering must still
    treat them as untrusted).
- `crates/shell-host/src/model.rs` — added `SnapshotView` (plain data,
  no schema import) + `ShellModel::apply_snapshot_view`; nothing else
  touched.
- `crates/shell-host/src/lib.rs` — `pub mod control` + doc line.
- 10 in-module tests, all socketpair + in-test fake server (schema
  `encode_frame` on the server end), no live compositor: hello
  roundtrip, stale-major (typed `Error` AND undecodable `Hello`
  paths), snapshot-into-model, delta apply + revision bump,
  gap-triggered resnapshot (local mismatch + compositor `RevisionGap`
  signal, incl. re-hello bytes on the wire and flag clearing),
  activation id correlation (wire command asserts + `CommandResult`
  echo + id increment), no-selection refusal, idle-poll `WouldBlock`.

## Verify (rustc 1.98.1)

- `cargo fmt -p rwd-shell-host` — clean
- `cargo clippy -p rwd-shell-host --all-targets -- -D warnings` — clean
- `cargo test -p rwd-shell-host` — 19 passed, 0 failed
- Not committed. `Cargo.lock` / `Cargo.toml` / `crates/compositor/**`
  entries in the working tree are sibling streams' work — untouched.

## Open seams for later streams

- Compositor must answer re-`Hello` with a fresh `Snapshot`
  (snapshot-on-reconnect); workspace removal has no explicit opcode
  (shadow restates by id, model workspace set only grows).
- Activation-result -> `select_window` + overview hide belongs to the
  panel UI wiring, which also owns the toggle-guard timing from the
  wave-2 notes.
