# Dependencies (body)

- greetd 0.10.3 (adopted daemon): PAM conversation + session launch;
  no changes, pinned for protocol/config reference only.
- `rwd-compositor` Supervisor shape: reused as a pattern (read-only);
  no changes to the compositor.
- Overlay state-machine pattern (`overlay.rs`): list/select model
  reused by analogy; no changes.
- GTK4/libadwaita system libraries: greeter UI runtime; new system
  dependency, must be present in greeter/CI environments.
- 003 session/logind work and 004 auth/lock design: planning
  dependencies — the greeter ships after both land (spec constraint,
  later milestone).
- Design documents: none referenced (spec carries no design refs;
  declared design sources are empty).
