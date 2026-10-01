# Stream 1 notes — versioned shell control protocol

Owner: control-schema. Owns `crates/shell-control-schema/**` + this file only.
Spec: 001 "Initial control API" (`001-nested-compositor-shell-recovery.md`);
ADR: `docs/adr/0002-shell-control-ipc-envelope.md`.

## Reference survey (read-only /tmp clones, patterns only — no GPL code copied)

Surveyed 2026-09-28. All implementation below is original code.

### niri-ipc (`/tmp/niri-survey/niri-ipc/`)

- Versioning policy: the crate follows the niri release version and is
  explicitly NOT semver API-stable; consumers must pin an exact version —
  `niri-ipc/src/lib.rs:37-44`.
- Version discovery is a protocol-level request, not a handshake field:
  `Request::Version` (`lib.rs:68-69`) answered by `Response::Version(String)`
  (`lib.rs:140-141`).
- Version-mismatch handling is diagnostic, not a reject: on reply parse
  failure the CLI re-asks `Request::Version` and prints a
  compositor-vs-CLI hint (`src/ipc/client.rs:74-90`, `:103-124`).
- Framing: JSON, one message per line, over a Unix socket whose path comes
  from `$NIRI_SOCKET` (`socket.rs:12`, `send` at `socket.rs:51-63`,
  line-delimited reads at `socket.rs:89-101`).
- State sync pattern adopted in spirit: the event stream always delivers the
  full current state up-front, so a fresh subscriber never needs a separate
  query (`lib.rs:104-112`); client state is rebuilt by folding events
  (`state.rs:17-29` trait `EventStreamStatePart::replicate/apply`,
  `EventStreamState` at `state.rs:31-50`).
- What we did differently (per ADR 0002): binary postcard body with a
  u32-LE length prefix instead of JSON lines (compact, bounded, no line
  scanning); explicit `ProtocolVersion{major,minor}` handshake with a hard
  major-mismatch reject instead of exact-release pinning; 1 MiB cap checked
  before decoding.

### cosmic-comp (`/tmp/cosmic-survey/src/`)

- Shell state is exposed via Wayland protocol globals, not a side socket:
  ext-workspace-v1 groups/handles plus cosmic workspace v2
  (`wayland/protocols/workspace/ext.rs:1-21`).
- Full-state-on-bind pattern: on `bind`, the compositor pushes every group
  to the new client then `done()` (`ext.rs:62-80`, push helper at
  `ext.rs:289`) — the same snapshot-on-connect semantics our `Snapshot`
  uses, transported differently.
- Untrusted text handled by change-only push: toplevel titles are re-sent
  only when `window.title()` differs from cached state
  (`wayland/protocols/toplevel_info.rs:506-575`); the title accessor is
  part of the toplevel info trait (`toplevel_info.rs:36`).
- Taken as confirmation for: compositor-owned state with full snapshot on
  connect, titles as untrusted text, revision/gap resync done via fresh
  snapshot rather than catch-up replay.

## Design record (this crate)

- `ProtocolVersion{major:u16, minor:u16}`, `CURRENT = 0.1`;
  `is_compatible_with` = same major && peer minor <= ours.
- `Message`: Hello{version}, Snapshot{revision, windows, workspaces},
  Changes{from_revision, to_revision, ops}, Command{id, kind},
  CommandResult{id, status}, Error{kind, message}.
- Ids are plain `u64` aliases (`WindowId`, `WorkspaceId`); one-use/expiry/
  seat policy lives in compositor code per ADR 0002, not here.
- `ActivationToken(String)` newtype with redacted `Debug` (spec R7);
  `TokenMeta{purpose, app_id: Option<String>, issued_at_ms: u64}`.
- Framing: `encode_frame` = u32-LE len + postcard body (grown `to_slice`
  buffer; core-only postcard API, so no feature/Cargo.toml change was
  needed). `decode_frame` checks prefix → 1 MiB cap (before postcard) →
  completeness → no trailing bytes → titles → Hello major.
- `DecodeError`: Truncated / Oversize / Malformed(postcard::Error) /
  IncompatibleVersion / TitleTooLong (`MAX_TITLE_LEN` = 512 bytes).
- Tests (16, in-module): roundtrips for all six variants (+ all three
  `CommandKind`s, both `CommandStatus`es), CURRENT value, compat matrix,
  oversize-before-decode, truncated (short prefix/empty/cut body),
  trailing-bytes malformed, stale-major (1.1 and 9.0), title cap
  reject/boundary/`Changes`-op, token redaction.

## Verify (package-scoped only)

- `cargo fmt -p rwd-shell-control`
- `cargo clippy -p rwd-shell-control --all-targets -- -D warnings`
- `cargo test -p rwd-shell-control` → 16 passed, 0 failed (2026-09-28,
  rustc 1.98.1, postcard 1.1.3).
