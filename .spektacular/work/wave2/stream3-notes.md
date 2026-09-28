# Wave 2 Stream 3 notes — supervised shell host skeleton (`shell-host`)

Spec: 001 R3 (Activities trigger + window list) and R5 (crash isolation,
bounded restart, resnapshot) in
`.spektacular/specs/20260927170317-a01f0011-001-nested-compositor-shell-recovery.md`;
supervision per `docs/adr/0003-nested-supervision-and-recovery.md`
(compositor spawns the shell as a child, finite restart budget with capped
backoff, child-only `WAYLAND_DISPLAY`, recovery via compositor-owned
overlay in slice 1).

## Reference survey (read-only; behavior notes only, no code copied)

Clones (shallow, under `/tmp`; both GPL — patterns observed, all
implementation below is original):

- `/tmp/gnome-shell-survey` — gnome-shell, commit `0062bde` as cloned
  2026-09-28 (`git clone --depth 1 https://github.com/GNOME/gnome-shell.git`)
- `/tmp/cosmic-panel-survey` — cosmic-comp, as cloned 2026-09-28
  (`git clone --depth 1 https://github.com/pop-os/cosmic-comp.git`;
  pre-existing `/tmp/cosmic-survey` is the same upstream, commit `9f746ea`)

### GNOME — Activities trigger and overview toggle

- `js/ui/panel.js:198` — `ActivitiesButton` is a toggle button
  (`accessible_role: TOGGLE_BUTTON`, `accessible_name: 'Activities'`,
  `:222-226`) with Return/Space key bindings (`:204-215`). Click and
  keyboard both funnel into one guard-then-toggle path: `:241-243`
  (click gesture) and `_toggleAction` (`:266-273`).
- Guard before every toggle: `shouldToggleByCornerOrButton`
  (`js/ui/overview.js:440-449`) refuses while an animation is in
  progress, while a drag is active, or within the activation-timeout
  window after a hot-corner trigger (prevents open/close double-fire).
- `toggle()` (`js/ui/overview.js:598-607`) is a pure state flip:
  visible → `hide()`, else → `show()`. `show` (`:486-501`) is a no-op
  when already shown; `hide` (`:539-560`) is a no-op when already
  hidden and ignores Ctrl-click. Show grabs modal input (`_syncGrab`,
  `:451-481`); hide releases it.
- Window activation from the overview hides it (`_onDragMotion`,
  `:310-325`: activate window, then `hide()`).
- Taken for our skeleton: single toggle entry point, idempotent
  show/hide, activation dismisses the overview. Our `ShellModel` keeps
  the same shape at slice-1 scale: `toggle_overview` flip +
  `select_window` by id (activation request itself goes through
  compositor policy later — R3, ADR 0002 token rule).

### COSMIC — panel placement and overview state

- Panel identity: `src/state.rs:166-171` special-cases the panel client
  (`com.system76.CosmicPanel`) — panels are known, privileged layer
  clients, not ordinary toplevels. Mirrors our choice of a distinct
  layer-shell namespace (`rwd-shell-panel`).
- Layer stacking: `src/shell/mod.rs:3077` treats `Layer::Top |
  Layer::Overlay` as the above-windows band — our panel binds
  `Layer::Top`, the same band GNOME/COSMIC panels occupy.
- Overview as explicit state machine: `src/shell/mod.rs:144-149`
  (`OverviewMode::None / Started / Active / Ended` with animation
  timestamps, `:151-159`). Our slice-1 analogue is the single
  `overview_open` flag; the animated state machine is deferred, not
  rejected.
- Applets note: cosmic-comp carries no in-tree panel/applets — the
  panel is the out-of-tree `cosmic-panel` client speaking layer-shell
  to the compositor. That client/compositor split is exactly the
  001 constraint "shell is a separate Wayland client; no UI logic in
  the compositor", which this skeleton follows.

## What was built (this stream owns `crates/shell-host/**` only)

- `crates/shell-host/src/lib.rs` — crate docs; `pub mod model, panel`.
  (Lib+bin split so the model/panel API is a real public surface for
  the later control-protocol adapter instead of `#[allow(dead_code)]`
  scaffolding; binary stays thin.)
- `crates/shell-host/src/main.rs` — binary entry: runs the default
  panel config, prints the error, exits nonzero on failure.
- `crates/shell-host/src/model.rs` — `ShellModel` (plain types, std
  only, no dependency on other workspace crates): `WindowEntry { id:
  u64, title: String, active: bool }`, `workspaces: Vec<u32>`
  (sorted/deduped), `overview_open`; `select_window` (false + no-op on
  empty/unknown), `apply_window_list` (first-active-wins, snapshot never
  flips overview), `toggle_overview`. 8 in-module unit tests cover the
  required edges (empty list, unknown id, overview with no windows).
- `crates/shell-host/src/panel.rs` — Wayland client setup against the
  exact workspace pins (wayland-client 0.31.15, wayland-protocols
  0.32.13, wayland-protocols-wlr 0.3.12): `connect_to_env` (i.e.
  `WAYLAND_DISPLAY`), `registry_queue_init`, bind `wl_compositor` +
  `zwlr_layer_shell_v1`, top-anchored (`Top|Left|Right`) `Layer::Top`
  surface with panel-height exclusive zone and `OnDemand` keyboard
  interactivity, configure-ack + commit, run-until-closed loop.

## Deferred runtime (explicit)

Our test compositor has no layer-shell yet, so live run is deferred by
design: without `zwlr_layer_shell_v1` the binary exits with
`PanelError::NoLayerShell` rather than falling back to a misplaced
surface role. Structure (connect → bind → top-anchored surface → ack →
dispatch) matches the protocol and compiles clean. Bring-up happens
with the compositor's layer-shell support, not here.

## Merge seams for later streams

- Control-protocol adapter (R4/ADR 0002): translate `Snapshot`/ordered
  changes into `apply_window_list`, activation results into
  `select_window`; compositor-issued interaction tokens required.
- Supervision (ADR 0003): compositor spawns this binary, owns restart
  budget/backoff; crash == process exit, resync from full snapshot.

## Verify (mandated commands only, rustc 1.98.1)

- `cargo fmt -p rwd-shell-host` — clean
- `cargo clippy -p rwd-shell-host --all-targets -- -D warnings` — clean
- `cargo test -p rwd-shell-host` — 9 passed, 0 failed
- `cargo check -p rwd-shell-host` — clean
- `Cargo.toml` / `Cargo.lock` md5-verified unchanged by this stream;
  nothing committed.
