# tuna-shell-gtk

The Tuna Desktop shell drawn with GTK4 and libadwaita (ADR 0006): GNOME 51's top
panel as a layer-shell strip, the calendar and notification list, quick
settings, the overview's dash, search and app grid, the Alt+Tab switcher,
OSDs, the lock screen, and the window menu. Every control is a real GTK
widget, so it uses the system interface font and is reachable over AT-SPI.

## Binary

`tuna-shell-gtk` (`src/main.rs`). This crate has no library target and no
public Rust API. The compositor runs it as its supervised shell when it is
installed beside it, otherwise falls back to `tuna-shell-host`; select it
explicitly with `TUNA_SHELL_BIN=tuna-shell-gtk` or
`tuna-compositor --shell-bin`. Compositor state arrives over the shell
control socket (`TUNA_CONTROL_SOCKET`).

Modules in `src/` map to GNOME surfaces and services, for example
`calendar`, `events`, `notify`, `services` (NetworkManager, BlueZ, power
profiles, brightness, volume), `wifi`, `bt_menu`, `network_agent`,
`overview`, `folders`, `providers` (D-Bus search providers), `switcher`,
`lock`, `power`, `screenshot_ui`, and `tray`. `build.rs` compiles GNOME
Shell's icons from `icons/` into a GResource.

## Depends on

`tuna-shell-host` (app discovery, control client, notifications store,
tray watcher), `tuna-shell-control`, GTK4, libadwaita, gtk4-layer-shell
(not packaged on Ubuntu 24.04; see `scripts/ci-install-gtk4-layer-shell`)
and gtk4-session-lock. Proven end to end by `scripts/tuna-gtk-shell-proof`
and `scripts/tuna-scale-proof`.

## Docs

[ADR 0006](../../docs/adr/0006-gtk4-libadwaita-shell.md),
[parity ledger](../../docs/parity-ledger.md),
[pixel parity with GNOME 51](../../docs/gnome-parity.md),
[keymap](../../docs/keymap.md),
[running tests locally](../../docs/testing.md).
