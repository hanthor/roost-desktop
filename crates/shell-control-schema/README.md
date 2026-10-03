# roost-shell-control

The versioned private IPC protocol between the compositor (authoritative)
and the shell: major/minor version negotiation, a full snapshot on every
(re)connect, ordered incremental changes, request-ID-bearing commands, and
compositor-issued activation tokens.

Wire format (ADR 0002): a `u32` little-endian length prefix followed by a
postcard body. Frames larger than `MAX_FRAME_BYTES` (1 MiB) are rejected
before decoding.

## Public API (`src/lib.rs`)

- Framing: `encode_frame`, `decode_frame`, `DecodeError`, `MAX_FRAME_BYTES`.
- Versioning: `ProtocolVersion`, `CURRENT_VERSION`.
- Messages: `Message`, `StateOp`, `CommandKind`, `CommandStatus`,
  `ErrorKind`.
- State types: `WindowInfo`, `WorkspaceInfo`, `OutputInfo`, `PreviewInfo`,
  `InputSettings`, `WindowId`, `WorkspaceId`, `dynamic_workspace_count`.
- Tokens and secrets: `ActivationToken`, `TokenMeta`, `Secret`.
- Switcher and keybindings: `SwitcherAction`, `SwitcherKey`,
  `SwitcherThumbnail`, `WindowAction`, `Accelerator`, and the `MOD_*` and
  `MODE_*` constants.

Its only dependencies are `postcard` and `serde`. Tests are inline in
`src/lib.rs`.

## Dependents

`roost-compositor` (server side), `roost-shell-host` (client in `control`),
and `roost-shell-gtk`.

## Docs

[ADR 0002](../../docs/adr/0002-shell-control-ipc-envelope.md) (envelope and
activation tokens),
[ADR 0005](../../docs/adr/0005-control-socket-peer-authentication.md)
(control-socket peer authentication),
[architecture](../../docs/architecture.md).
