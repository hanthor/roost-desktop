---
created_date: "2026-09-28"
document_status: final
closed_date: "2026-09-28"
---

# Plan: 20260928093027-31646037-greetd-greeter

<!-- Metadata -->
<!-- Created: 2026-09-28T10:53:29Z -->
<!-- Commit: 25dc81f -->
<!-- Branch: main -->
<!-- Repository: https://github.com/hanthor/rust-wayland-desktop.git -->

## Overview

Build the RWD login greeter: a graphical login screen that authenticates
through the adopted system login service and launches any installed
session. It gives RWD machines cold-boot graphical login without
rebuilding password handling, as a later milestone after the session
and identity groundwork lands.

## Conventions

No always-applied project conventions are configured; the working
standards for this plan are the house verification loop (`cargo fmt`,
`clippy -D warnings`, `cargo test`, all enforced in CI), exact
workspace version pins per ADR 0001, and original code only when
referencing GPL upstream (GNOME, niri, Cosmic).

## Architecture & Design Decisions

### Options weighed

**A. Smithay-native custom greeter UI.** Hand-rendered widgets on the
RWD compositor. Full control and no toolkit dependency, but a bespoke
widget set plus a hand-rolled accessibility bridge at high cost, and
it diverges from the modern GNOME look the goal demands. Effort: High.

**B. GTK4/libadwaita greeter hosted by the RWD compositor (chosen).**
The greeter runs as the sole pre-login client. Modern GNOME look,
AT-SPI, keyboard nav, and HIG patterns come with the toolkit; the
gtkgreet shape proves it works. Accepts a short-term two-toolkit
coexistence with the undecided shell toolkit (002 gate stays open).
Effort: Medium.

**C. Pluggable frontends (graphical + TTY).** Rejected as scope creep;
TTY fallback duplicates the adopted daemon's own ecosystem. Effort:
High.

### Decision
Option B. The goal names modern GNOME-like login; GTK4 delivers it
plus accessibility option A cannot cheaply match. This covers the
greeter surface only — 002 weighs it as input for the shell decision.

### Requirement-to-files map
Sign-in, session choice, prompts, failure, and cold-boot behaviors
all land in the single registered repo (`rust-wayland-desktop`):
the new `rwd-greeter` crate carries model, client, enumerator, and UI;
the Supervisor pattern is reused read-only from `rwd-compositor`;
fixtures and the fake daemon harness live under the greeter's tests.

## Component Breakdown

- **Greeter app (new, `rwd-greeter`)**: the login screen process. Owns
  the user list, session picker, prompt rendering, and failure
  notices. Talks to the adopted daemon over JSON-IPC; renders with
  GTK4/libadwaita as a fullscreen client of the pre-login compositor.
- **Prompt state machine (new, inside greeter)**: turns the daemon's
  auth conversation into UI states (prompt → answer → next prompt /
  success / generic failure). Owns the no-hardcoded-password rule;
  every prompt shape renders generically.
- **Session enumerator (new, inside greeter)**: reads installed session
  files into picker entries with RWD default. Owns fixture-driven
  parsing, never live system paths in tests.
- **Session supervisor (reused pattern)**: watches the launched
  session process using the Supervisor shape from `rwd-compositor`;
  on early exit, returns to the login screen with a notice. Owns the
  dead-session recovery behavior.
- **Fake daemon harness (new, tests only)**: scripted JSON-IPC peer
  standing in for greetd. Owns deterministic auth/session exchanges
  so no test touches PAM.

## Data Structures & Interfaces

- `Prompt`: one auth question from the daemon — id, echo/non-echo
  style, message text. The unit the state machine renders and answers.
- `AuthExchange`: ordered prompts plus terminal outcome
  (success with session command, or generic failure). Never carries
  distinguishing failure detail.
- `SessionEntry`: name, exec command, desktop-file source, and
  is-default flag. Produced by the enumerator from session files.
- `GreeterModel`: selected user, selected session, prompt queue,
  notice text, screen state (user-pick / prompting / failed /
  launching). The single state the UI renders from.
- Greeter↔daemon boundary: the adopted daemon's JSON-IPC conversation
  (create-session → post-auth-message* → start; cancel on failure).
  Exact wire shapes verified against greetd 0.10.3 at implementation.
- Greeter→session boundary: spawned session command observed by the
  supervisor; early exit maps to the dead-session notice, never to
  daemon internals leaking into the UI.

## Implementation Detail

- New crate `rwd-greeter` (binary + lib): prompt state machine and
  session enumeration live in the lib for headless testing; the binary
  wires GTK4 views to the model.
- New pattern: scripted-daemon harness — a fake JSON-IPC peer in
  tests, following the house fake-server pattern, so no test touches
  PAM or needs a display.
- Greeter UI follows GTK4/libadwaita login-window patterns (user list,
  password prompt, session picker, error notice) per the gnome-gui
  skill at implementation time; no custom widget framework.
- Supervisor reuse: session-process monitoring mirrors the
  `rwd-compositor` Supervisor shape (spawn/poll/budget) without
  depending on it.
- Session files parsed from fixtures in tests and from system session
  directories at runtime; RWD default when present.

## Dependencies

- greetd 0.10.3 (adopted daemon): PAM conversation + session launch;
  no changes, pinned for protocol/config reference only.
- `rwd-compositor` Supervisor shape: reused as a pattern (read-only);
  no changes to the compositor.
- Overlay state-machine pattern: list/select model reused by analogy;
  no changes.
- GTK4/libadwaita system libraries: greeter UI runtime; new system
  dependency, must be present in greeter/CI environments.
- 003 session/logind work and 004 auth/lock design: planning
  dependencies — the greeter ships after both land (spec constraint,
  later milestone).
- Design documents: none referenced (spec carries no design refs;
  declared design sources are empty).

## Testing Approach

- Unit tests on the prompt state machine (all prompt shapes, failure
  collapsing, cancel paths) and session-file parsing (fixtures,
  malformed entries, RWD default) — the most covered components,
  because auth UX bugs are the costliest here.
- Contract tests against the fake daemon: full exchanges (success,
  bad password, multi-prompt, session crash) asserting the model
  transitions and never a shell drop.
- Spec success metrics become behavioral tests where headless-safe
  (sign-in flow, picker contents, failure notice) and flagged manual
  VM checks where not (cold boot, real PAM).
- Follows the house loop: `cargo fmt`, `clippy -D warnings`,
  `cargo test`, enforced in CI; GTK UI code included in all three.
- Deliberate gap: no live-PAM or cold-boot tests in CI — unmockable
  secrets and hardware; covered by scripted VM runs at acceptance.

## Milestones & Tasks

### M1 — Headless login conversation works
The greeter completes full auth exchanges against the fake daemon:
user pick, password and multi-factor prompts, success handoff, and
generic failure — all proven by contract tests with no display.
Validation: fake-daemon suite green, including bad-password and
crashed-session cases.

### M2 — Session picker lists real sessions
Installed session files appear in the picker with RWD default, parsed
from fixtures in tests and system paths at runtime. Validation:
fixture tests plus a manual run listing this machine's sessions.

### M3 — Graphical login screen
The GTK login window renders users, prompts, picker, and notices from
the proven model, keyboard-navigable end to end. Validation: scripted
walkthrough against the fake daemon under a nested compositor, plus
the HIG pass from the gnome-gui skill.

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

## Open Questions

- Exact greetd 0.10.3 JSON wire shapes for edge exchanges (cancel
  mid-prompt, session-start failure payloads) — verified against the
  protocol doc at implementation; the fake models the documented
  subset until then.
- Whether the pre-login compositor host needs layer-shell or
  fullscreen-shell for the GTK greeter window — decided when the
  nested walkthrough task exercises it.
- Precise GTK4/libadwaita system packages for CI images — recorded
  during the UI task from a real apt resolution.

## Out of Scope

- The greetd daemon itself: adopted at 0.10.3, never vendored or
  rebuilt in this plan.
- Production seat/identity design (004) and session/logind
  implementation (003); the plan consumes their future outputs.
- Live PAM, cold-boot automation, distro packaging, remote login,
  certified accessibility journeys, theming systems.
