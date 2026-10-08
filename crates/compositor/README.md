# roost-compositor

The long-lived Smithay compositor: owns the Wayland display, windows,
workspaces, input, outputs, the session lock, and the supervised shell
child. Runs nested (winit, GLES over EGL) or as a DRM/KMS hardware session.

## Binaries

- `roost-compositor` (`src/main.rs`): one session on a private socket;
  `--help` lists the flags (`--backend auto|winit|drm`, `--socket`,
  `--shell-bin`, `--xwayland`, `--startup-overview`).
- `roost-session` (`src/bin/roost-session.rs`): what greeters and
  `wayland-sessions/roost.desktop` run; execs the sibling compositor.

## Public API (`src/lib.rs`)

- `State` (the protocol-handler state) and `TestCompositor`, a harness that
  drives the display by hand so integration tests run without an event loop
  or GPU; `SEAT_NAME`.
- Modules: `runtime` (session runtime, backend choice, shell binary
  resolution), `windows`, `overview`, `popup`, `layer`, `protocols`,
  `control` and `state` (control channel and revisioned state model),
  `supervise` and `overlay` (shell supervision and recovery overlay),
  `lock`, `session_lock`, `unlock`, `pam`, `drm` (feature `drm`),
  `xwayland`, `monitors`, `wallpaper`, and GNOME D-Bus services
  (`mutter`, `screencast`, `screenshot`, `introspect`, `idle_monitor`).

Features: `drm` and `xwayland`, both on by default.

## Depends on / dependents

Uses `roost-shell-control`, `roost-greeter-control` (prompt model and greetd
client), and `roost-wallpaper`. `roost-shell-host` uses it as a dev-dependency
for its live tests. Integration tests are in `tests/`.

## Docs

[Architecture](../../docs/architecture.md),
[ADR 0001](../../docs/adr/0001-nested-backend-smithay-pin.md) (Smithay pin),
[ADR 0002](../../docs/adr/0002-shell-control-ipc-envelope.md),
[ADR 0003](../../docs/adr/0003-nested-supervision-and-recovery.md),
[ADR 0005](../../docs/adr/0005-control-socket-peer-authentication.md),
[nested session](../../docs/nested-session.md),
[protocols](../../docs/protocols.md), [keymap](../../docs/keymap.md).
