# Component Breakdown (body)

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
