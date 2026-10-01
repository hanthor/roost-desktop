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
| P-OV-01 | Super opens the overview with live window previews for the current workspace | partial | journey:J-OV-OPEN, proof:G-OV-OPEN, proof:G-OV-PICK, cargo:spread_previews_never_overlap_and_stay_in_the_card | compositor-drawn workspace card with scaled live previews; click picks (`crates/compositor/src/overview.rs`) | visuals unverified against baseline shots |
| P-OV-02 | Overview shows the workspace strip with thumbnails; drag or click switches | missing |  | workspaces exist in the model and switch by key; no strip | — |
| P-OV-03 | Overview shows the dash with favorites and running apps | partial | proof:G-OV-SHELL-UI | GTK shell dash with favorites (`crates/shell-gtk/src/overview.rs`); running-app indicators missing | — |
| P-OV-04 | Typing in the overview searches apps and launches the top hit with Enter | partial | journey:J-OV-SEARCH, journey:J-OV-LAUNCH, proof:G-OV-SEARCH, proof:G-OV-LAUNCH, proof:G-SEARCH-PROVIDER, proof:G-SEARCH-ACTIVATE, proof:G-SEARCH-VIEW, proof:G-APP-INSTALLED, cargo:selection_follows_gnome_settings | app search plus GNOME Shell D-Bus search providers with their settings keys (`crates/shell-gtk/src/providers.rs`) | — |
| P-OV-05 | App grid with pages and folders | partial | proof:G-OV-APPGRID | Show Apps opens a grid of installed apps; no pages or folders | — |
| P-OV-06 | Escape returns to the previously focused window | pass (liveness only) | cargo:overview_parks_keyboard_focus_and_restores_on_dismiss, journey:J-OV-ESC | `overview_parks_keyboard_focus_and_restores_on_dismiss` | needs baseline comparison |
| P-PN-01 | Top panel: Activities, centered clock, system menu right | partial | proof:G-PANEL, proof:A11Y-PANEL | GTK shell panel matches the layout (ADR 0006); legacy shell still the default | — |
| P-PN-02 | Clock opens calendar plus notification list | partial | proof:G-CAL-OPEN, proof:G-NOTIFY-LIST, proof:G-NOTIFY-CLEAR | GTK shell calendar popover lists notifications with Clear (`crates/shell-gtk/src/notify.rs`) | no events column |
| P-PN-03 | Quick settings grid: Wi-Fi, Bluetooth, power mode, night light, dark style, volume, brightness | partial | proof:G-QS-OPEN, proof:A11Y-QS, proof:G-QS-SERVICES, proof:G-QS-WIFI, proof:G-QS-BLUETOOTH, proof:G-QS-POWER, proof:G-QS-DARK, proof:G-QS-BRIGHTNESS, proof:G-QS-VOLUME, proof:G-QS-ABSENT, cargo:power_mode_follows_gnome_51 | GTK shell grid drives NetworkManager, BlueZ, power-profiles-daemon, logind brightness, PipeWire volume, and the GSettings tiles; each tile hides without its daemon (`crates/shell-gtk/src/services.rs`); proven against stub daemons | no per-tile menus (Wi-Fi network list, Bluetooth devices, output picker) |
| P-NT-01 | Notification banners stack top-center with actions and dismissal | partial | cargo:panel::tests::live::banners_surface_appears_and_destroys_with_queue, cargo:intake::tests::live_bus::stub_notify_shows_a_banner_and_close_notification_dismisses_it, proof:G-NOTIFY-BANNER, proof:G-NOTIFY-EXPIRE | GTK shell banners top-center with actions, close, and expiry | style unverified against baseline |
| P-NT-02 | Do Not Disturb toggle and notification history | partial | cargo:notifications::tests::dnd_gates_banners_but_not_critical_or_history, proof:G-NOTIFY-DND, proof:G-NOTIFY-LIST | DND in quick settings and the calendar holds banners back; history listed | needs baseline comparison |
| P-WM-01 | Alt-Tab switcher with app icons and window previews | partial | cargo:alt_tab_steps_commits_and_cancels_without_leaking_keys, journey:J-SW-STEP, proof:G-SWITCHER | GTK shell draws GNOME's centered icon strip with the selected title; Alt release focuses it | per-window, not grouped by app; no window previews |
| P-WM-02 | Super+Up maximizes, Super+Left/Right tiles halves | pass (liveness only) | cargo:super_arrows_drive_layouts_and_consume | `super_arrows_drive_layouts_and_consume`; `roost-app-content` tiles Chromium | needs baseline comparison |
| P-WM-03 | Dynamic workspaces, Super+PgUp/PgDn switch, Shift moves window | pass (liveness only) | cargo:super_page_keys_switch_and_shift_moves_focused, journey:J-WS-NEXT | `super_page_keys_switch_and_shift_moves_focused` | — |
| P-WM-04 | Interactive drag-to-move and resize with pointer | partial | cargo:header_drag_moves_the_window, cargo:bottom_right_resize_grows_and_configures, cargo:super_drag_moves_without_a_client_request, proof:G-APP-DRAG | header drag, edge resize, Super+drag, top-edge maximize (`windows.rs`) | half-tile snapping missing |
| P-WM-06 | App menus and popovers open as xdg popups and close on outside click | partial | cargo:popup_is_configured_where_the_positioner_asked, cargo:press_on_another_client_dismisses_the_grab_and_is_consumed, proof:G-APP-POPOVER, proof:G-CAL-OPEN | xdg popups with positioner constraints and the grab rule (`crates/compositor/src/popup.rs`) | — |
| P-WM-05 | Window close, minimize, maximize via server-side or client decorations | partial |  | close via Alt+F4 and dock; no decorations policy | — |
| P-LK-01 | Idle blanks then locks; password unlocks; nothing leaks while locked | pass (nested) | cargo:lock_command_engages_and_unlock_restores, cargo:correct_password_dismisses_lock_intact, cargo:pam_unlock_accepts_the_right_password_and_nothing_else, proof:G-SUSPEND-LOCK | `tests/lock.rs`, `tests/unlock.rs`; unlock through greetd, else PAM (`roost-lock` service); locks before sleep | no separate blank stage before the lock |
| P-SY-01 | Session starts from the display manager on hardware | partial | proof:D-OUTPUT, proof:D-FRAMES, proof:D-SHELL, cargo:pam_unlock_accepts_the_right_password_and_nothing_else | DRM/KMS on vkms in CI (`scripts/roost-drm-smoke`); the Marlin preview lists "Roost (preview)" at GDM with PAM unlock and logout | no hardware or VM login run yet (#68) |
| P-SY-02 | Multi-monitor with per-output panel and hotplug | pass (nested) | cargo:hotplug_remove_and_readd_round_trips_without_restart | `tests/outputs.rs`, `tests/migration.rs` | real outputs untested |
| P-SY-03 | Fractional scaling and mixed DPI | partial | proof:S-LOGICAL, proof:S-PANEL, proof:S-WINDOW, proof:S-INPUT, proof:D-SCALE, cargo:picks_the_arrangement_for_the_lit_connectors | per-output scale and layout from GNOME's monitors.xml on hardware; `ROOST_SCALE` for nested sessions; logical layout, fractional-scale clients, scaled input and cursor | fractional clients use the primary output's scale on every output (#59) |
| P-SY-04 | XWayland apps run by default | partial | proof:G-X11-APP, cargo:on_by_default_when_xwayland_is_installed | default builds include X11; a session starts XWayland when installed and gives launched apps DISPLAY; X11 windows get GNOME placement and focus | XWayland starts with the session, not on the first X11 connection as in Mutter (#59) |
| P-SY-06 | Clients find the protocols Mutter offers (dmabuf, activation, viewporter, fractional scale, cursor shape, idle inhibit) | partial | cargo:advertised_globals_match_the_golden_list, cargo:dmabuf_is_advertised_only_with_renderer_formats, cargo:focused_client_can_hand_focus_with_a_token, cargo:background_clients_and_serial_less_tokens_cannot_steal_focus, proof:G-GL-WINDOW | `docs/protocols.md` lists what is advertised and each gap; Mutter column read from source, not captured | baseline `wayland-info` capture open (#89) |
| P-SY-07 | End session: Suspend, Restart…, Power Off…, Log Out with GNOME's 60 s confirmation dialog | partial | proof:G-POWER-MENU, proof:G-POWER-DIALOG, proof:G-POWER-OFF, cargo:nested_sessions_only_offer_log_out | quick-settings power menu over logind (`crates/shell-gtk/src/power.rs`); nested preview offers Log Out only | proven against a logind stub; hardware run pending the VM lane (#68) |
| P-SY-05 | Screen capture and sharing through portals with consent | partial | proof:G-SCREENSHOT, proof:G-SCREENSHOT-BUTTON, proof:G-SCREENCAST, cargo:paths_follow_gnome | screenshots (org.gnome.Shell.Screenshot) and monitor screen casts (org.gnome.Mutter.ScreenCast with DisplayConfig, PipeWire) — what xdg-desktop-portal-gnome calls, niri's shape | window casts, dmabuf zero-copy, cursor metadata and GNOME's interactive screenshot UI are missing (#61) |
| P-A11Y-01 | Screen reader reads panel, overview, quick settings | partial | proof:A11Y-PANEL, proof:A11Y-QS, proof:A11Y-GOLDEN | GTK shell exposes every control by name; golden trees in `tests/a11y/` fail CI on a dropped or renamed control (`scripts/roost-a11y-golden`) | GNOME 51 trees for comparison come with the baseline capture (#64) |
| P-A11Y-02 | Every pointer action has a keyboard equivalent | partial | cargo:panel::tests::live::key_only_run_opens_launches_and_activates | keymap and key-only run (`docs/keymap.md`) | chords not rebindable |
| P-IN-01 | IME composition in GTK apps | partial | cargo:an_input_method_types_into_the_focused_text_field | text-input v3 and input-method v2 relay commits; candidate popups tracked | preedit drawing and IBus (GNOME's default IME) untested; needs an IME in the VM lane (#68) |
| P-IN-02 | Touchpad gestures (three-finger overview, workspace swipe) | partial | cargo:three_finger_swipes_follow_gnome | three-finger swipe up/down toggles the overview, sideways switches workspace; other swipes reach apps | acts on release, without GNOME's follow-the-fingers animation; hardware run pending (#68) |
| P-ST-01 | Settings written by GNOME Settings take effect (wallpaper, clock, fonts) | partial | cargo:panel::tests::live::wallpaper_survives_session_restart, proof:G-QS-DARK, proof:G-SETTINGS-IDLE, proof:G-SETTINGS-INPUT, proof:G-SETTINGS-CLOCK, proof:G-DISPLAY-SETTINGS, cargo:input_sources_become_one_keymap, cargo:idle_lock_follows_gnome_keys | `docs/settings-map.md` lists every GNOME 51 key as honored, via GTK, or ignored with an owner | keybindings not yet honored (#63) |
| P-TR-01 | AppIndicator tray items show icons and menus | pass (nested) | cargo:watcher::tests::live_bus::stub_item_serves_icon_and_menu_over_private_bus, proof:G-TRAY | `watcher.rs` tests now use the spec's reply types; GTK shell tray runs the watcher on a worker thread | GNOME 51 needs an extension for this; Roost ships it natively (deviation, accepted) |

## Tests column

Comma-separated references, checked by `scripts/roost-ledger` in CI
(the `parity-ledger` job):

- `cargo:<name>` names a workspace test, by full path or by its last
  segment (`cargo test -- --list`). The `check` job runs every test, so a
  listed test is a passing test.
- `journey:<ID>` names a state assertion recorded as `<ID> pass` in a
  journey's `assertions.txt` (see `scripts/lib/roost-introspect.sh`).
- `proof:<ID>` names a stage recorded as `<ID> pass` by the GTK shell
  proof (`scripts/roost-gtk-shell-proof`) or the DRM smoke test
  (`scripts/roost-drm-smoke`).

A row whose status starts with `pass` must cite at least one test, and
every cited test must exist and pass. CI fails otherwise.

## How to update

Add a row when a GNOME 51 behavior enters scope. Move a row to `pass` only
with a cited test or a recorded review entry (`docs/reviews/`). A row whose
cited test disappears or fails breaks CI.
