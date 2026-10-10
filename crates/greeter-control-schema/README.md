# tuna-greeter-control

The shared schema and IPC client for the greetd conversation: prompt state
machine, greetd JSON-IPC client, and session types.

## Public API (`src/lib.rs`)

- `client`: `GreeterClient`, blocking greetd conversation driver over a Unix stream.
- `model`: `GreeterModel`, `ModelEvent`, `Prompt`, `Screen`, `SessionRef`.
- Re-exports of `GreeterClient` and `GreeterModel` at crate root.

Its only dependency is `greetd_ipc`.

## Dependents

`tuna-greeter` and `tuna-compositor`.
