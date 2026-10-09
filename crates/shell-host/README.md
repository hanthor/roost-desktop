# tuna-shell-host

The original Tuna Desktop shell, a supervised Wayland client that draws its own
pixels (panel, overview, dock, tray, notification banners), plus the
toolkit-free shell logic the GTK shell reuses.

## Binary

`tuna-shell-host` (`src/main.rs`): the compositor spawns it as a
supervised child with `WAYLAND_DISPLAY` and `TUNA_CONTROL_SOCKET` set
for the child only. It attaches to `zwlr_layer_shell_v1` and exits with an
error if the compositor does not offer it. `scripts/tuna-nested run` starts
this shell; packaged sessions prefer `tuna-shell-gtk` when it is installed
(`SHELL_BINARIES` in `crates/compositor/src/runtime.rs`).

## Public API (`src/lib.rs`)

- `model`, `panel`, `overview`, `popup`, `dock`, `tiles`: view state and
  the self-drawn surfaces.
- `control`: client for the shell control protocol (hello, snapshot,
  ordered changes with gap resnapshot, token activation).
- `apps`, `search`, `favorites`, `icons`, `keyboard`: desktop entries,
  search, launching, pinned favorites, icon theme lookup, xkb input.
- `intake`, `notifications`, `watcher`: the freedesktop notifications bus
  edge and store, and the StatusNotifier (AppIndicator) watcher.
- `settings`, `prefs`, `xdg`: GNOME settings snapshot, Tuna Desktop prefs,
  fail-closed XDG directories.
- `extensions`: sandboxed Rhai extension host.
- `introspect`: read-only state snapshot for the proof scripts.

## Dependents

`tuna-shell-gtk` uses `apps`, `control`, `favorites`, `intake`, `model`,
`notifications` and `watcher`. Live tests in `tests/` run against
`tuna-compositor`.

## Docs

[ADR 0003](../../docs/adr/0003-nested-supervision-and-recovery.md),
[ADR 0006](../../docs/adr/0006-gtk4-libadwaita-shell.md),
[extensions](../../docs/extensions.md),
[settings map](../../docs/settings-map.md),
[nested session](../../docs/nested-session.md).
