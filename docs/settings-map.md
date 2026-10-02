# GNOME settings compatibility map

Roost reads GNOME 51's own GSettings schemas. It ships no shim schemas
and runs no translating daemon. The Marlin image already carries
`gsettings-desktop-schemas`, so the keys exist, and GNOME Settings and
`gsettings` keep working unchanged. Ledger row P-ST-01 tracks this page.

This page records that decision for R9 (knowledge entry
`learnings/dconf-settings-interop.md`).

**Status words.**
- **Honored**: Roost reads the key and reacts while it runs.
- **Via GTK**: GTK or libadwaita reads the key for every app and for the
  shell, so nothing in Roost has to.
- **Ignored**: the key has no effect yet. The reason and owner are given.

## org.gnome.desktop.interface

| Key | Status | Notes |
|---|---|---|
| color-scheme | Honored | The Dark Style tile writes it. The shell and apps follow it through libadwaita. Proof G-QS-DARK |
| accent-color | Via GTK | libadwaita applies it |
| font-name, document-font-name, monospace-font-name | Via GTK | The shell is GTK, so its text follows too |
| text-scaling-factor | Via GTK | |
| gtk-theme, icon-theme, cursor-theme, cursor-size | Via GTK | Inside apps. The compositor's own cursor ignores them (#89) |
| clock-format | Honored | Panel clock |
| clock-show-weekday, clock-show-date, clock-show-seconds | Honored | Panel clock, built as gnome-desktop's wall clock builds it |
| enable-hot-corners | Honored | Live. Proof G-SETTINGS-INPUT |
| enable-animations | Ignored | Roost has no animations yet |
| show-battery-percentage | Ignored | No battery indicator yet (#55) |

## Session, lock and notifications

| Schema and key | Status | Notes |
|---|---|---|
| org.gnome.desktop.session idle-delay | Honored | Live, through the shell's `SetIdleTimeout` command. Proof G-SETTINGS-IDLE |
| org.gnome.desktop.screensaver lock-enabled, lock-delay | Honored | The lock engages `idle-delay + lock-delay` after the last input. Roost has no separate blank stage |
| org.gnome.desktop.screensaver picture-uri | Ignored | The lock screen has no background image |
| org.gnome.desktop.notifications show-banners | Honored | Mirrors Do Not Disturb both ways. Proof G-NOTIFY-DND |
| org.gnome.desktop.search-providers (all keys) | Honored | disable-external, disabled, enabled and sort-order. Proof G-SEARCH-PROVIDER |
| org.gnome.desktop.background picture-uri, picture-uri-dark | Honored | Wallpaper |
| org.gnome.shell favorite-apps | Honored | Used for the dash when Roost has no pins of its own |
| org.gnome.desktop.app-folders (folder-children and each folder's name, apps, categories, excluded-apps, translate) | Honored | App-grid folders, read on each grid open. Proof G-APP-FOLDERS |
| org.gnome.shell enabled-extensions | Ignored | GNOME Shell extensions are JavaScript. Roost has its own extension point |
| org.gnome.settings-daemon.plugins.color night-light-enabled | Honored (write only) | The Night Light tile writes it, but Roost applies no colour temperature yet (#89 gamma control) |

## Displays

| File | Status | Notes |
|---|---|---|
| ~/.config/monitors.xml | Honored | The arrangement for the lit connectors sets each output's scale, position and primary monitor (hardware sessions). Proof D-SCALE |
| GNOME Settings' Displays panel | Honored | ApplyMonitorsConfig over org.gnome.Mutter.DisplayConfig changes scale and position live; "keep changes" saves to ~/.config/roost/monitors.xml, which Roost reads before GNOME's file (GNOME's own file is never rewritten). Proof G-DISPLAY-SETTINGS |

## Window management and input

| Schema and key | Status | Notes |
|---|---|---|
| org.gnome.shell.keybindings toggle-overview, toggle-application-view, toggle-message-tray, toggle-quick-settings, switch-to-application-1..9, open-new-window-application-1..9, screenshot, screenshot-window, screen-brightness-up, screen-brightness-down | Honored | read live, GNOME 51's defaults when the schema is missing; grabbed through the compositor like any accelerator (`crates/shell-gtk/src/keybindings.rs`) |
| org.gnome.shell.keybindings show-screenshot-ui, show-screen-recording-ui, focus-active-notification, shift-overview-up/down, the per-monitor brightness keys | Ignored | no screenshot or recording UI, notification focus, or per-monitor brightness yet (#63) |
| org.gnome.desktop.wm.keybindings, org.gnome.mutter.keybindings | Ignored | Roost uses GNOME's default window-manager bindings, fixed (#63) |
| org.gnome.desktop.wm.preferences button-layout | Via GTK | Client-side decorations read it |
| org.gnome.desktop.wm.preferences focus-mode, num-workspaces | Ignored | Click to focus and dynamic workspaces, as GNOME's defaults |
| org.gnome.mutter dynamic-workspaces, edge-tiling | Ignored | Always on, as GNOME's defaults |
| org.gnome.desktop.peripherals.keyboard repeat, delay, repeat-interval | Honored | Seat key repeat, live. Proof G-SETTINGS-INPUT |
| org.gnome.desktop.peripherals.touchpad tap-to-click, natural-scroll, speed, disable-while-typing | Honored | libinput on hardware sessions, live and on hotplug |
| org.gnome.desktop.peripherals.mouse natural-scroll, speed | Honored | libinput on hardware sessions |
| org.gnome.desktop.input-sources sources, xkb-options | Honored | xkb sources become one keymap in order; Super+Space switches; IBus sources are skipped (#60). Proof G-SETTINGS-INPUT |
| org.gnome.desktop.a11y.* | Via GTK | Where GTK implements them. Compositor features like zoom are missing |
