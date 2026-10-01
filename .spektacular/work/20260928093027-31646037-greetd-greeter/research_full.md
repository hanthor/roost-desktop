---
created_date: "2026-09-28"
document_status: draft
---

# Research: 20260928093027-31646037-greetd-greeter

## Alternatives considered and rejected
- Smithay-native custom greeter UI: full control, no toolkit; rejected
  — bespoke widgets plus a hand-rolled AT-SPI bridge at high cost,
  diverging from the modern GNOME look the goal demands.
- Pluggable graphical+TTY frontends: rejected as scope creep — TTY
  fallback duplicates the adopted daemon's own ecosystem.
- Rebuilding the login daemon: rejected — re-auditing PAM/session
  handling for zero gain over adopting greetd 0.10.3.

## Chosen approach — evidence
GTK4/libadwaita greeter hosted by the RWD compositor (gtkgreet and
cosmic-greeter prove the daemon+GTK shape in production); AT-SPI,
keyboard nav, and HIG patterns come with the toolkit. Covers the
greeter surface only — shell toolkit (002 gate) stays open.

## Files examined
- `crates/compositor/src/supervise.rs` (Supervisor spawn/poll/budget)
- `crates/compositor/src/overlay.rs` (list/select state machine)
- `crates/shell-host/` (layer-shell client setup, fake-server tests)
- `crates/shell-control-schema/` (framing/versioning discipline)
- `.spektacular/work/wave2/*-notes.md` (upstream reference surveys)

## External references
- greetd 0.10.3 (kennylevinsen/greetd, GPL-3.0-only): daemon adopted,
  pinned for protocol/config reference.
- GNOME GUI skill (in-workspace skill): login-window patterns at
  implementation time.
- gtkgreet / cosmic-greeter: production evidence for the shape.

## Prior plans / specs consulted
- Spec `20260928093027-31646037-greetd-greeter` (final): scope,
  requirements, constraints, success metrics.
- 001 nested spec + ADRs 0001 (pins), 0003 (supervision/recovery),
  0004 (monorepo seams — new crate stays in-workspace).
- Knowledge: `learnings/dconf-settings-interop.md` (no hardcoded
  theme/font sources).

## Open assumptions
- greetd JSON-IPC wire details verified at implementation from the
  protocol doc, not memory.
- Pre-login host needs layer-shell vs fullscreen-shell decided in
  the nested walkthrough task.
- CI system packages for GTK4 resolved during the UI task.

## Drafting assumptions
- greetd 0.10.3 pinned for protocol/config reference; daemon never vendored.
- No live PAM/cold-boot in CI; fake greetd server + fixtures in tests, VM/manual for A-criteria.
- GTK4 choice covers the greeter surface only; shell toolkit (002 gate) stays open.

## Rehydration cues
Plan work dir: `.spektacular/work/20260928093027-31646037-greetd-greeter/`
(research, architecture, assumptions, per-section drafts). Interview:
multi-session picker, later-milestone gate. Wire-shape and shell-host
questions resolve in tasks T2/T5.
