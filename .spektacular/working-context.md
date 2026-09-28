# Working context — greetd-greeter spec

## Session direction
User asked to reimplement GDM on modern tech ("i want to reimplement
GDM also on modern tech"). Agreed approach before speccing: adopt
greetd as the PAM/session daemon, build only the RWD greeter UI.

## Key decisions
- GDM decomposed into 5 jobs: (1) PAM conversation + session launch,
  (2) greeter UI, (3) seat/session tracking (003 territory),
  (4) user switching / lock integration (004 territory),
  (5) distro integration (006 territory).
- Adopt, don't rebuild, the daemon: greetd is a minimal Rust
  Wayland-native login daemon with JSON-IPC daemon↔greeter split,
  used by cosmic-greeter and niri sessions. Rebuilding PAM/session
  handling buys an audit burden for zero gain.
- License is clean: repo is GPL-3.0-or-later; greetd is GPL-3.0-only,
  so even linking its IPC crate is compatible.
- New spec `20260928093027-31646037-greetd-greeter`: adopt-greetd
  daemon + RWD greeter UI, daemon explicitly out of scope. Follow-on
  to 001, with 003/004 dependencies.

## User's exact phrasing
- "i want to reimplement GDM also on modern tech"
- "yes please dl" (confirming: capture as spec, adopt-greetd + RWD
  greeter UI)

## Related session state
- Wave 3 merged (control channel, shell client, recovery harness,
  overlay model; 76 tests green, pushed as 25dc81f).
- Pending elsewhere: /tmp 71G disk investigation; harness stream notes
  already merged.

## New goal: implement the greeter spec
- User set goal "implement this spec" for
  20260928093027-31646037-greetd-greeter (modern-DE login, spec tech
  stack). Plan walked through, approved, set final; plan workflow
  finished. Implement read_plan gate passed (structure, drift,
  spec coverage clean; first-task mode). Current task: prompt model
  + daemon client (T1).
