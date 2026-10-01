# ADR 0005: Control-socket peer authentication

- Status: Decided (implemented; closes #30)
- Date: 2026-10-01
- Owner: project
- Dependent speks: 001 (control API), 004 (secure session)

## Context

The control socket is privileged: a session can focus and close windows,
drive the overview, and engage the lock. It accepted every connection
that could reach it, and its path fell back to the shared temp dir when
`XDG_RUNTIME_DIR` was unset (#30). Activation tokens did not help: they
are minted into snapshots delivered over this same channel.

## Decision

Three layers, none of which needs a protocol change:

1. **Location.** The socket lives only in `XDG_RUNTIME_DIR`. Bind fails
   when that is unset, or when the directory is not owned by us with
   mode `0700`. The socket file is `0600`.
2. **Authentication.** Every accepted peer's `SO_PEERCRED` is read before
   any byte of state crosses the socket. Another uid, or unreadable
   credentials, is refused (fail closed).
3. **Authorization.** Once the compositor supervises its shell, only the
   supervised child's pid may hold a session; between restarts nobody
   may. A shell restart under a new pid evicts sessions held by the old
   one. Pending, un-handshaken peers are capped at 8.

## Alternatives considered

- **Per-session secret in the environment.** Rejected: same-uid
  processes can read `/proc/<pid>/environ`.
- **Inherited pre-connected socketpair.** The strongest option, since no
  path exists to connect to. Deferred: it changes the reconnect model and
  every test harness. Revisit with the 004 production credential design.
- **Uid check alone.** Rejected: does not stop same-uid spoofing, which
  is the documented threat.

## Consequences

- A same-uid process that can `ptrace` the shell still wins; that is the
  platform's same-uid boundary, not something a socket can fix.
- Tests and unsupervised development use the `SameUser` gate.
- Evidence: `crates/compositor/tests/peer_auth.rs`.
