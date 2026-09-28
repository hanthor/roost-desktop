# Stream 4 notes — recovery overlay state machine

Owner: overlay-model. Owns `crates/compositor/src/overlay.rs` and this
file only.

Spec: 001 R5; ADR: `docs/adr/0003-nested-supervision-and-recovery.md`.
`RecoveryAction` comes from `crate::supervise` (Wave 2 stream 4) and is
not redefined here.

## Reference (GNOME locked-shell shield, behavior-level — original code)

From guidelines memory, no upstream clone or read: GNOME keeps a minimal
locked-shell shield up when the normal shell is unavailable, offering
only session-preserving actions while ordinary shell chrome stays out of
reach, with focus and input kept alive on the shield path. The overlay
follows that shape (compositor-served window list, shell relaunch,
overlay toggle) as pure state; pixels arrive with the winit backend
slice. All implementation here is original.

## Design (overlay.rs)

- `Overlay{visible, windows: Vec<OverlayWindow{id,title}>, selected:
  Option<u64>, restart_attempts_used, budget}` — selection is an id so
  it survives list refreshes; mutators keep it on a listed id or `None`.
- `show(windows)` selects the first window; `hide` keeps list/selection.
  `select_next`/`select_prev` wrap, are empty-safe, and seed from the
  ends when nothing is selected. `select(id)` ignores unknown ids.
  `refresh_from_snapshot(&[(u64, String)])` replaces the list, preserving
  the selection when its id still exists, else first-or-`None`.
- `record_restart` (saturating) counts against `budget`;
  `is_exhausted` is `used >= budget` (so a zero budget starts exhausted).
- `action_for(OverlayKey) -> Option<RecoveryAction>`: `Activate ->
  ListWindows`, `Relaunch -> RelaunchShell`, `Dismiss -> ShowOverlay`;
  `Up`/`Down` are navigation (via `select_next`/`select_prev`) -> `None`.

## Verify (package-scoped only)

- `cargo fmt -p rwd-compositor`
- `cargo clippy -p rwd-compositor --all-targets -- -D warnings`
- `cargo test -p rwd-compositor overlay`, then full
  `cargo test -p rwd-compositor` once green (rustc 1.98.1)
