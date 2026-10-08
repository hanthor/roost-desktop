# tuna-greeter

The Tuna Desktop login greeter for greetd: a prompt state machine, installed
session enumeration, a greetd JSON-IPC client, and a GTK4/libadwaita login
window rendered from the model. No password-specific flow exists anywhere
in the crate; it follows whatever conversation greetd drives.

## Binary

`tuna-greeter` (`src/main.rs`), built only with the `gtk-ui` feature
(on by default).

## Public API (`src/lib.rs`)

- `model`: `GreeterModel`, the prompt state machine that turns greetd's
  auth conversation into UI state.
- `session`: parse installed `.desktop` session files
  (`enumerate_system`).
- `client`: blocking greetd conversation driver over a Unix socket.
- `ui`: the login window (feature `gtk-ui`).

Feature `gtk-ui` (default) pulls in GTK4 and libadwaita. Consumers that
need only the model and client build with `default-features = false` and
need no GTK system libraries.

## Dependents

`tuna-compositor` uses `model` and `client` for its greetd unlock path,
with `gtk-ui` off. Tests in `tests/` drive a fake greetd.

## Docs

[Greeter VM acceptance](../../docs/greeter-vm-acceptance.md),
[installing Tuna Desktop](../../docs/install.md),
[architecture](../../docs/architecture.md).
