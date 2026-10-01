# Task context (plan context.md entries)

### Task: Prompt model and daemon client
Files: `crates/greeter/src/model.rs` (Prompt/AuthExchange/GreeterModel),
`crates/greeter/src/client.rs` (JSON-IPC conversation driver),
`crates/greeter/src/lib.rs` re-exports. Crate `rwd-greeter` (lib +
binary; binary thin until the UI task). Deps: serde_json for the
daemon wire (verify version at implementation; workspace pin then).
Complexity: medium — generic prompt rendering is the load-bearing
design point. Verify wire shapes against greetd 0.10.3 protocol doc
before freezing message types.

### Task: Fake daemon and contract tests
Files: `crates/greeter/tests/fake_greetd.rs` (scripted peer over a
socketpair), `crates/greeter/tests/exchanges.rs` (success, bad
password, multi-prompt, session crash). The fake speaks the same JSON
framing the client implements; diverge the scripts per case, never
the framing. Complexity: low-medium.

### Task: Session enumerator and fixtures
Files: `crates/greeter/src/session.rs`,
`crates/greeter/tests/fixtures/*.desktop` (valid, RWD default,
malformed, empty dir). Parser isini-style, tolerant: skip bad entries
with a count, never fail the whole read. Runtime paths
(`/usr/share/wayland-sessions`, `/usr/share/xsessions`) behind a
single `SESSION_DIRS` constant for test override. Complexity: low.

### Task: GTK login UI and HIG pass
Files: `crates/greeter/src/main.rs`, `crates/greeter/src/ui.rs`
(views bound to GreeterModel). Deps adding: gtk4 + libadwaita
(system libs; CI needs `libgtk-4-dev libadwaita-1-dev` — record exact
packages at implementation). Keyboard-only operation mandatory;
run the gnome-gui HIG checklist before sign-off. Complexity: high —
first GTK surface in the workspace.

### Task: Nested walkthrough and VM acceptance script
Files: `crates/greeter/tests/walkthrough.rs` (fake daemon + nested
compositor drive, CI-safe), `docs/greeter-vm-acceptance.md` (manual
cold-boot/PAM procedure). Complexity: medium — VM part is documented
procedure, not automation.
