---
created_date: "2026-09-28"
document_status: final
closed_date: "2026-09-28"
---

# Context: 20260928093027-31646037-greetd-greeter

## Per-Task Technical Notes

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
malformed, empty dir). Parser ini-style, tolerant: skip bad entries
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

## Testing Strategy
Per task: model/enumerator get unit tests (deterministic, fixtures);
daemon exchanges get contract tests against the scripted fake;
UI gets keyboard walkthrough + HIG checklist; walkthrough task adds
the nested drive plus the manual VM procedure. House loop
(fmt/clippy/test) covers all new code including GTK.

## Current State Analysis
Workspace holds compositor, control-schema, and shell-host crates
with 76+ passing tests; no greeter code exists. Reusable patterns:
Supervisor (spawn/poll/budget), overlay list/select model,
layer-shell client setup, fake-server test discipline. greetd is
absent from dev/CI environments by design decision.

## Project References
- Spec: `20260928093027-31646037-greetd-greeter` (final).
- ADRs: 0001 (pins), 0003 (supervision/recovery patterns),
  0004 (monorepo seams — new crate stays in-workspace).
- License: GPL-3.0-or-later workspace-wide; greetd GPL-3.0-only
  (compatible).
- Skills: gnome-gui (UI patterns at implementation).

## Token Management Strategy
GBus-free design: greeter talks JSON-IPC to the daemon only; no
D-Bus session dependencies in the login path. Test tokens are
scripted fixtures, never live credentials.

## Migration Notes
No existing login flow to migrate; 1.0 ships without graphical
login per spec constraint. When the greeter lands, document the
TTY-to-greeter handoff for installs that started headless.

## Performance Considerations
Login path is infrequent and human-paced; no latency budgets beyond
the spec's cold-boot-to-screen VM check. Keep the greeter binary
lean (no compositor code linked in) so pre-login startup stays fast.
