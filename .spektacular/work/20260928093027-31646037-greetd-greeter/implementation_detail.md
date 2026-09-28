# Implementation Detail (body)

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
