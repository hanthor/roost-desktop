# Tasks for plan.md

#### - [ ] Task: Prompt model and daemon client
**Id:** b8fd51df-a8b5-4c52-92ce-60c41d0445d5
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

New `rwd-greeter` crate with the prompt state machine and the
greetd JSON-IPC client: any prompt shape renders generically, failure
collapses to one notice, cancel paths return to user-pick.

*Technical detail:* Task: Prompt model and daemon client in context.md

**Acceptance criteria**:
- [ ] Unit tests cover password, multi-prompt, failure, and cancel flows
- [ ] No password-specific screen or flow exists in the model

#### - [ ] Task: Fake daemon and contract tests
**Id:** 3088e273-b12c-4004-9df3-7f7efd6d9315
**Repo:** rust-wayland-desktop
**Depends on:**
- b8fd51df-a8b5-4c52-92ce-60c41d0445d5 — Prompt model and daemon client
**Execution:** agent

Scripted fake greetd peer plus contract tests for success, bad
password, multi-prompt, and session-crash exchanges; no test touches
PAM or a display.

*Technical detail:* Task: Fake daemon and contract tests in context.md

**Acceptance criteria**:
- [ ] All four exchanges pass deterministically headless
- [ ] Wire shapes verified against greetd 0.10.3 protocol behavior

#### - [ ] Task: Session enumerator and fixtures
**Id:** 5100f12f-7c1f-45e6-97c6-b02a97834f8a
**Repo:** rust-wayland-desktop
**Depends on:** none
**Execution:** agent

Session-file parsing with RWD default, fixture-driven tests
including malformed entries, runtime reads from system session
directories.

*Technical detail:* Task: Session enumerator and fixtures in context.md

**Acceptance criteria**:
- [ ] Every installed session file appears with RWD preselected
- [ ] Malformed entries are skipped without failing enumeration

#### - [ ] Task: GTK login UI and HIG pass
**Id:** 29592feb-e1ee-40d1-9dd4-16725ca5cd25
**Repo:** rust-wayland-desktop
**Depends on:**
- b8fd51df-a8b5-4c52-92ce-60c41d0445d5 — Prompt model and daemon client
- 5100f12f-7c1f-45e6-97c6-b02a97834f8a — Session enumerator and fixtures
**Execution:** agent

GTK4/libadwaita login window (user list, prompts, picker, notices)
rendered from the proven model, keyboard-navigable, HIG-reviewed via
the gnome-gui skill.

*Technical detail:* Task: GTK login UI and HIG pass in context.md

**Acceptance criteria**:
- [ ] Full sign-in walkthrough keyboard-only against the fake daemon
- [ ] HIG pass reports no blocking violations

#### - [ ] Task: Nested walkthrough and VM acceptance script
**Id:** 222784c7-bc00-4505-b7e1-550e112269ea
**Repo:** rust-wayland-desktop
**Depends on:**
- 29592feb-e1ee-40d1-9dd4-16725ca5cd25 — GTK login UI and HIG pass
**Execution:** agent

Scripted walkthrough driving the greeter under a nested compositor
plus a documented VM procedure for cold-boot and real-PAM
acceptance (manual run, scripted setup).

*Technical detail:* Task: Nested walkthrough and VM acceptance script in context.md

**Acceptance criteria**:
- [ ] Scripted nested walkthrough passes in CI-safe environments
- [ ] VM procedure documents cold-boot and real-credential runs
