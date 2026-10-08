# roost-greeter

The Roost login greeter for greetd: a prompt state machine, installed
session enumeration, a greetd JSON-IPC client, and a GTK4/libadwaita login
window rendered from the model. No password-specific flow exists anywhere
in the crate; it follows whatever conversation greetd drives.

## Binary

`roost-greeter` (`src/main.rs`), built only with the `gtk-ui` feature
(on by default).

## Public API (`src/lib.rs`)

- `model`: `GreeterModel`, the prompt state machine (re-exported from
  `roost-greeter-control`).
- `session`: parse installed `.desktop` session files
  (`enumerate_system`).
- `client`: blocking greetd conversation driver over a Unix socket
  (re-exported from `roost-greeter-control`).
- `ui`: the login window (feature `gtk-ui`).

Feature `gtk-ui` (default) pulls in GTK4 and libadwaita. `roost-greeter-control`
holds the headless prompt state machine and IPC client with no GTK dependencies.

## Dependents

`roost-greeter` provides the login window and binary; `roost-compositor`
depends on `roost-greeter-control` directly for its unlock path. Tests in
`tests/` drive a fake greetd.

## Docs

[Greeter VM acceptance](../../docs/greeter-vm-acceptance.md),
[installing Roost](../../docs/install.md),
[architecture](../../docs/architecture.md).
