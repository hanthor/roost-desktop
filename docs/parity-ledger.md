# Parity ledger

**Baseline:** GNOME 51 as shipped in the TunaOS Marlin GNOME image
(`ghcr.io/tuna-os/marlin:gnome`). Record the image digest used for each
baseline capture in the evidence column.
**Rule:** a row is `pass` only when a test or recorded review compares Roost
against baseline evidence. `untested` is not a pass. Undocumented gaps are
not parity (see roadmap change control).

Status values: `pass`, `partial`, `missing`, `deviation` (deliberate, with
rationale and owner), `untested`.

| ID | GNOME 51 behavior | Roost status | Tests | Evidence / notes | Deviation and owner |
|---|---|---|---|---|---|
| P-OV-01 | Super opens the overview with live window previews for the current workspace | missing | journey:J-OV-OPEN | overview lists windows as a text/box list only (`crates/shell-host/src/overview.rs`) | — |
| P-OV-02 | Overview shows the workspace strip with thumbnails; drag or click switches | missing |  | workspaces exist in the model and switch by key; no strip | — |
| P-OV-03 | Overview shows the dash with favorites and running apps | partial |  | bottom dock exists outside the overview (`dock.rs`) | layout differs; owner TBD |
| P-OV-04 | Typing in the overview searches apps and launches the top hit with Enter | partial | journey:J-OV-SEARCH, journey:J-OV-LAUNCH | `scripts/roost-journey` proves launch via marker file; no search providers | — |
| P-OV-05 | App grid with pages and folders | missing |  | — | — |
| P-OV-06 | Escape returns to the previously focused window | pass (liveness only) | cargo:overview_parks_keyboard_focus_and_restores_on_dismiss, journey:J-OV-ESC | `overview_parks_keyboard_focus_and_restores_on_dismiss` | needs baseline comparison |
| P-PN-01 | Top panel: Activities, centered clock, system menu right | partial |  | clock and three tiles; small bitmap font; no Activities label | — |
| P-PN-02 | Clock opens calendar plus notification list | partial |  | calendar popup rows exist; no notification list | — |
| P-PN-03 | Quick settings grid: Wi-Fi, Bluetooth, power mode, night light, dark style, volume, brightness | partial |  | network radio, sound, power rows (`tiles.rs`); no grid, no Bluetooth/brightness/night light/dark style | — |
| P-NT-01 | Notification banners stack top-center with actions and dismissal | partial | cargo:panel::tests::live::banners_surface_appears_and_destroys_with_queue, cargo:intake::tests::live_bus::stub_notify_shows_a_banner_and_close_notification_dismisses_it | banners exist and act (`notifications.rs`); position and style unverified against baseline | — |
| P-NT-02 | Do Not Disturb toggle and notification history | missing | cargo:notifications::tests::dnd_gates_banners_but_not_critical_or_history | history persists; no DND, no list surface | — |
| P-WM-01 | Alt-Tab switcher with app icons and window previews | partial | cargo:alt_tab_steps_commits_and_cancels_without_leaking_keys, journey:J-SW-STEP | switcher steps and commits; visuals unverified | — |
| P-WM-02 | Super+Up maximizes, Super+Left/Right tiles halves | pass (liveness only) | cargo:super_arrows_drive_layouts_and_consume | `super_arrows_drive_layouts_and_consume`; `roost-app-content` tiles Chromium | needs baseline comparison |
| P-WM-03 | Dynamic workspaces, Super+PgUp/PgDn switch, Shift moves window | pass (liveness only) | cargo:super_page_keys_switch_and_shift_moves_focused, journey:J-WS-NEXT | `super_page_keys_switch_and_shift_moves_focused` | — |
| P-WM-04 | Interactive drag-to-move and resize with pointer | missing |  | manager API only; no interactive drag (`windows.rs` header) | — |
| P-WM-05 | Window close, minimize, maximize via server-side or client decorations | partial |  | close via Alt+F4 and dock; no decorations policy | — |
| P-LK-01 | Idle blanks then locks; password unlocks; nothing leaks while locked | pass (nested) | cargo:lock_command_engages_and_unlock_restores, cargo:correct_password_dismisses_lock_intact | `tests/lock.rs`, `tests/unlock.rs` | greetd unlock path; PAM binding is a 004 gate |
| P-SY-01 | Session starts from the display manager on hardware | missing |  | winit backend only; no DRM/KMS | gate between tier 1 and 2 |
| P-SY-02 | Multi-monitor with per-output panel and hotplug | pass (nested) | cargo:hotplug_remove_and_readd_round_trips_without_restart | `tests/outputs.rs`, `tests/migration.rs` | real outputs untested |
| P-SY-03 | Fractional scaling and mixed DPI | missing |  | — | — |
| P-SY-04 | XWayland apps run by default | partial |  | behind the `xwayland` feature; off by default | — |
| P-SY-05 | Screen capture and sharing through portals with consent | missing |  | no portal backend, no screencopy | — |
| P-A11Y-01 | Screen reader reads panel, overview, quick settings | missing |  | no AT-SPI bridge (`docs/keymap.md`) | blocked on toolkit decision |
| P-A11Y-02 | Every pointer action has a keyboard equivalent | partial | cargo:panel::tests::live::key_only_run_opens_launches_and_activates | keymap and key-only run (`docs/keymap.md`) | chords not rebindable |
| P-IN-01 | IME composition in GTK apps | missing |  | no text-input / input-method protocols | — |
| P-IN-02 | Touchpad gestures (three-finger overview, workspace swipe) | missing |  | — | — |
| P-ST-01 | Settings written by GNOME Settings take effect (wallpaper, clock, fonts) | partial | cargo:panel::tests::live::wallpaper_survives_session_restart | wallpaper and clock format round-trip (`settings.rs`); no font/theme/scale | settings compat map open |
| P-TR-01 | AppIndicator tray items show icons and menus | pass (nested) | cargo:watcher::tests::live_bus::stub_item_serves_icon_and_menu_over_private_bus | `watcher.rs` tests | GNOME 51 needs an extension for this; Roost ships it natively (deviation, accepted) |

## Tests column

Comma-separated references, checked by `scripts/roost-ledger` in CI
(the `parity-ledger` job):

- `cargo:<name>` names a workspace test, by full path or by its last
  segment (`cargo test -- --list`). The `check` job runs every test, so a
  listed test is a passing test.
- `journey:<ID>` names a state assertion recorded as `<ID> pass` in a
  journey's `assertions.txt` (see `scripts/lib/roost-introspect.sh`).

A row whose status starts with `pass` must cite at least one test, and
every cited test must exist and pass. CI fails otherwise.

## How to update

Add a row when a GNOME 51 behavior enters scope. Move a row to `pass` only
with a cited test or a recorded review entry (`docs/reviews/`). A row whose
cited test disappears or fails breaks CI.
