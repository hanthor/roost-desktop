# Research — greetd-greeter plan (discovery)

## Target repo
Single-member project: `rust-wayland-desktop` at
`/home/ubuntu/dev/rust-wayland-desktop`. All work lands here.

## Always-applied knowledge
None configured (`knowledge always-applied` returned empty). No
conventions/glossary to reflect beyond the repo's own docs.

## Design references
Spec carries none; `design sources` is empty. Stated explicitly for
the Dependencies step.

## Topic knowledge
Searched greeter/login/session/pam: only hit is
`learnings/dconf-settings-interop.md` (marginal — greeter must not
hardcode theme/font sources; reuse the prompt-driven requirement).

## Codebase assets (read, reused as patterns)
- `crates/compositor/src/supervise.rs`: Supervisor spawn/poll/budget —
  session-process monitoring reuses this shape (greeter is spawned BY
  greetd; the greeter supervises the session handoff the same way).
- `crates/compositor/src/overlay.rs`: list/select/visibility state
  machine — the login screen's user/session lists follow this pattern.
- `crates/shell-host`: layer-shell Wayland client setup — greeter is a
  fullscreen privileged surface, same client discipline.
- `crates/shell-control-schema`: framing/versioning discipline is the
  house pattern; greetd IPC itself is JSON (daemon-side, adopted).
- GNOME GUI skill loaded for greeter UI patterns (libadwaita,
  prompt/error states); behavior reference only.

## Gaps found
- greetd 0.10.3 latest tag (kennylevinsen/greetd, GPL-3.0-only,
  compatible). Not installed on dev machine or CI: integration tests
  must run against a fake greetd server (mirrors our fake-server test
  pattern), never a live PAM stack.
- No `/usr/share/wayland-sessions` entries here: session enumeration
  needs file fixtures in tests.
- Cold-boot and PAM flows are VM/manual acceptance (A-criteria),
  not CI-runnable.

## Decisions
- Pin greetd 0.10.3 for config/protocol reference; daemon adopted,
  never vendored.
- New crate `rwd-greeter` (binary + lib for testable model); Wayland
  client via existing workspace pins; layer-shell or fullscreen-shell
  decided at architecture step.
- No new workspace pins beyond greetd reference; no DBus/PAM crates
  in the greeter (daemon owns that side).
