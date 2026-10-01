# Architecture & Design Decisions — greetd-greeter plan

## Options weighed

### A. Smithay-native custom greeter UI
Fullscreen client rendered by hand (GLES/shm widgets) on the RWD
compositor. Pros: zero toolkit dependency; full visual control;
consistent with the compositor stack. Cons: bespoke widget set to
build and maintain; accessibility needs a hand-rolled AT-SPI bridge
(high cost, 004 risk); diverges from the "modern GNOME" look the
goal demands. Effort: High.

### B. GTK4/libadwaita greeter hosted by the RWD compositor (chosen)
Greeter is a GTK4/libadwaita app run as the sole client of the RWD
compositor pre-login. Pros: modern GNOME look for free; AT-SPI,
keyboard nav, and HIG patterns come with the toolkit (gnome-gui
skill loaded); gtkgreet proves the shape. Cons: GTK4/libadwaita
system deps in the greeter environment; shell toolkit decision (002
gate) stays open, so two toolkits may coexist short-term — accepted
explicitly, revisit if the shell chooses otherwise. Effort: Medium.

### C. Pluggable frontends (graphical + TTY fallback)
Prompt model core with swappable UIs. Pros: covers degraded
environments. Cons: two frontends to build/test; TTY fallback
duplicates greetd's own tuigreet ecosystem. Rejected as scope creep.
Effort: High.

## Decision
Option B. Rationale: the goal names a modern GNOME-like login; GTK4
gives it plus accessibility that option A cannot cheaply match. The
02 toolkit gate is not prejudged — this decision covers the greeter
surface only, recorded here so 002 weighs it as input, not accident.

## Requirement-to-files map (single repo: rust-wayland-desktop)
- Sign-in/session-choice/prompts/failure/cold-boot behaviors →
  `crates/greeter/` (new crate `rwd-greeter`: `src/main.rs`,
  `src/model.rs` prompt state machine, `src/session.rs` session-file
  enumeration, `src/client.rs` greetd JSON-IPC conversation).
- Dead-session recovery → `crates/greeter/src/recovery.rs` reusing
  `rwd-compositor` Supervisor patterns (supervise.rs, read-only).
- Session fixtures → `crates/greeter/tests/fixtures/*.desktop`.
- Fake greetd server → `crates/greeter/tests/fake_greetd.rs`
  (JSON-IPC scripted from greetd 0.10.3 protocol behavior).
- Login-screen UI states → GTK4 views; HIG via gnome-gui skill at
  implementation time.

## Assumptions (also in assumptions.md)
- greetd 0.10.3 pinned for protocol/config reference; daemon never
  vendored or rebuilt.
- No live PAM/cold-boot in CI: fake-server tests + VM/manual
  acceptance per spec A-criteria.
- greetd JSON-IPC wire details verified at implementation (protocol
  doc read then), not assumed from memory here.
