# GNOME settings compatibility map

Tuna Desktop reads GNOME 51's own GSettings schemas. It ships no shim schemas
and runs no translating daemon.

The [exhaustive desktop inventory](settings-desktop-inventory.md) lists
every one of the 429 GNOME 51 desktop-schema keys. The sections below
explain common keys and related Shell/Mutter/settings-daemon settings. The Marlin image already carries
`gsettings-desktop-schemas`, so the keys exist and GNOME Settings can write them. Schema presence does not
establish functional compatibility: [the GNOME Settings audit](gnome-settings-audit.md)
tracks ineffective controls and unqualified service integrations. Ledger row P-ST-01 tracks this page.

This page records that decision for R9 (knowledge entry
`learnings/dconf-settings-interop.md`).

**Status words.**
- **Honored**: Tuna Desktop reads the key and reacts while it runs.
- **Via GTK**: GTK or libadwaita reads the key for every app and for the
  shell, so nothing in Tuna Desktop has to.
- **Ignored**: the key has no effect yet. The reason and owner are given.

## org.gnome.desktop.interface

| Key | Status | Notes |
|---|---|---|
| color-scheme | Honored | The Dark Style tile writes it. The shell and apps follow it through libadwaita. Proof G-QS-DARK |
| accent-color | Via GTK | libadwaita applies it |
| font-name | Honored | Live shell CSS family, base size, weight, style and stretch (with semantic emphasis preserved) and GTK client font settings; proof G-SETTINGS-FONT |
| document-font-name, monospace-font-name | Ignored by shell | Applications may choose to read these; Tuna Desktop chrome uses font-name |
| text-scaling-factor | Via GTK | Live GTK Wayland DPI translation; proof G-SETTINGS-FONT |
| gtk-theme, icon-theme, cursor-theme, cursor-size | Via GTK | Inside apps. The compositor's own cursor ignores them (#89) |
| clock-format | Honored | Panel clock |
| clock-show-weekday, clock-show-date, clock-show-seconds | Honored | Panel clock, built as gnome-desktop's wall clock builds it |
| enable-hot-corners | Honored | Live. Proof G-SETTINGS-INPUT |
| enable-animations | Honored | Live idle fade, GTK transitions and compositor overview/strip motion; disabled transitions finish immediately. Proofs G-ANIMATIONS-OFF, G-INTROSPECT-MOTION |
| show-battery-percentage | Honored | UPower DisplayDevice panel percentage; updates live |

GNOME 51 `org.gnome.desktop.a11y.interface reduced-motion` is read live and combines with `enable-animations` into one motion policy shared by the shell and compositor: `full`, `fade-only` or `off`. `enable-animations=false` is `off`: every transition finishes immediately. `reduce` with animations enabled is `fade-only`, as in GNOME 51: opacity fades (the idle shield) keep running, while translation and scale (compositor overview and strip motion, the shell's GTK transitions) finish immediately. `enable-animations` itself, and Introspect's `AnimationsEnabled`, stay true under Reduced Motion, as GNOME reports them. GNOME Shell's `GNOME_SHELL_SLOWDOWN_FACTOR` environment variable scales every duration as GNOME's `adjustAnimationTime` does. Older schema sets keep the existing animation policy. This controls the desktop shell; application toolkits remain responsible for their own transitions. Proofs G-ANIMATIONS-OFF, G-INTROSPECT-MOTION.

## Session, lock and notifications

| Schema and key | Status | Notes |
|---|---|---|
| org.gnome.desktop.session idle-delay | Honored | Live, through the shell's `SetIdleTimeout` command. Proof G-SETTINGS-IDLE |
| org.gnome.desktop.screensaver lock-enabled, lock-delay | Honored | Idle fades to black over ten seconds; lock waits for the larger of fade duration and lock-delay. Activity cancels blanking; an authenticated lock remains latched |
| org.gnome.desktop.screensaver picture-uri | Honored | Blurred, dimmed lock background; empty or missing files fall back to the desktop wallpaper |
| org.gnome.desktop.notifications show-banners | Honored | Mirrors Do Not Disturb live in both directions. Proof G-NOTIFY-DND and G-SETTINGS-NOTIFICATIONS (external writes and banner policy) |
| org.gnome.desktop.search-providers (all keys) | Honored | disable-external, disabled, enabled and sort-order. Proof G-SEARCH-PROVIDER |
| org.gnome.desktop.background picture-uri, picture-uri-dark | Honored | Wallpaper |
| org.gnome.shell favorite-apps | Honored | Used for the dash when Tuna Desktop has no pins of its own |
| org.gnome.desktop.app-folders (folder-children and each folder's name, apps, categories, excluded-apps, translate) | Honored | App-grid folders, read on each grid open. Proof G-APP-FOLDERS |
| org.gnome.shell enabled-extensions | Ignored | GNOME Shell extensions are JavaScript. Tuna Desktop has its own extension point |
| org.gnome.settings-daemon.plugins.color night-light-enabled | Honored (write only) | The Night Light tile writes it, but Tuna Desktop applies no colour temperature yet (#89 gamma control) |

## Displays

| File | Status | Notes |
|---|---|---|
| ~/.config/monitors.xml | Honored | The arrangement for the lit connectors sets each output's scale, position and primary monitor (hardware sessions). Proof D-SCALE |
| GNOME Settings' Displays panel | Partial | ApplyMonitorsConfig over org.gnome.Mutter.DisplayConfig changes scale and position live; "keep changes" saves to ~/.config/tuna/monitors.xml, which Tuna Desktop reads before GNOME's file (GNOME's own file is never rewritten). Proof G-DISPLAY-SETTINGS |

## Window management and input

| Schema and key | Status | Notes |
|---|---|---|
| org.gnome.shell.keybindings show-screenshot-ui, toggle-overview, toggle-application-view, toggle-message-tray, toggle-quick-settings, switch-to-application-1..9, open-new-window-application-1..9, screenshot, screenshot-window, screen-brightness-up, screen-brightness-down | Honored | read live, GNOME 51's defaults when the schema is missing; grabbed through the compositor like any accelerator (`crates/shell-gtk/src/keybindings.rs`) |
| org.gnome.shell.keybindings show-screen-recording-ui, focus-active-notification, shift-overview-up/down | Honored | recording UI; focus the current banner (Escape returns input); step session/picker/app grid in both directions; read and rebound live through shell grabs |
| org.gnome.shell.keybindings screen-brightness-up/down-monitor, screen-brightness-cycle(-monitor) | Honored | up/down/cycle through logind; monitor keys select the DRM connector under the pointer and never substitute another screen's backlight. Devices without a kernel connector association are unsupported and unchanged |
| org.gnome.settings-daemon.plugins.media-keys screensaver | Honored | Read live by the shell, with Super+L as the default; works without gsd-media-keys. Proof G-LOCK-KEY |
| org.gnome.settings-daemon.plugins.media-keys volume-up/down/mute, quiet/precise variants, mic-mute, their -static bindings, volume-step | Honored | Native async PipeWire controls and OSD, including while locked; reads both editable and static arrays live, 6% default / 2% precise. Range capped at 100%; feedback sounds and amplification are not implemented. Proof G-MEDIA-VOLUME / V-MEDIA-VOLUME |
| org.gnome.desktop.wm.keybindings activate-window-menu, toggle-maximized, maximize, unmaximize, minimize, close, begin-move, begin-resize, switch-input-source(-backward), switch-to-workspace-1/last/left/right, move-to-workspace-1/last/left/right; org.gnome.mutter.keybindings toggle-tiled-left/right | Honored | read live (GNOME 51's defaults without the schema), grabbed like any accelerator; once the shell grabs them the compositor's built-in defaults stand aside, so a rebound key moves and an emptied one reaches apps |
| org.gnome.desktop.wm.keybindings switch-applications(-backward), switch-group(-backward) | Honored | read live (GNOME 51's defaults without the schema) and handed to the compositor, which holds the switcher open while the chord's modifiers are held and commits when they are released; Shift steps backward, `Above_Tab` is the key above Tab |
| org.gnome.desktop.wm.keybindings switch-windows(-backward), cycle-windows(-backward), cycle-group(-backward) | Honored | compositor drives bound keys through SetSwitcherKeys: a window-only MRU popup (current workspace), or immediate window/app-group cycling without a popup. GNOME's switch-windows defaults are empty |
| org.gnome.desktop.wm.preferences button-layout | Via GTK | Client-side decorations read it |
| org.gnome.desktop.wm.preferences focus-mode, focus-new-windows, num-workspaces, auto-raise, auto-raise-delay, raise-on-click, mouse-button-modifier, resize-with-right-button | Honored | Click/sloppy/mouse focus, new-window focus, fixed workspace count (windows migrate to the last workspace), auto-raise with delay, raise on click, modifier drags and right-button resize; read live and sent to the compositor as `SetWmSettings` (#337) |
| org.gnome.desktop.wm.preferences audible-bell, visual-bell | Ignored | The bell always sounds (GNOME's default audible bell) and never flashes (xdg-system-bell, #89) |
| org.gnome.mutter dynamic-workspaces, workspaces-only-on-primary, edge-tiling, focus-change-on-pointer-rest, attach-modal-dialogs, center-new-windows | Honored | Fixed versus dynamic counts, primary-only workspaces, edge tiling, pointer-rest focus, attached modals and centered windows; read live and sent to the compositor as `SetWmSettings` (#337) |
| org.gnome.desktop.peripherals.keyboard repeat, delay, repeat-interval | Honored | Seat key repeat, live. Proof G-SETTINGS-INPUT |
| org.gnome.desktop.peripherals.touchpad tap-to-click, natural-scroll, speed, disable-while-typing | Honored | libinput on hardware sessions, live and on hotplug |
| org.gnome.desktop.peripherals.mouse natural-scroll, speed | Honored | libinput on hardware sessions |
| org.gnome.desktop.input-sources sources, xkb-options | Honored | xkb sources become one keymap in order; Super+Space switches; IBus sources use the supervised bridge (#60). Proof G-SETTINGS-INPUT |
| org.gnome.desktop.a11y.* | Partial / missing | Some keys reach GTK; zoom, screen keyboard, seat input aids and several modern preferences lack consumers. Spoken screen-reader acceptance remains unqualified; see the control-center audit |

`J-SETTINGS-LIVE` changes `clock-format` with `gsettings set` during the
nested journey and asserts the shell’s consumed settings snapshot before
restoring it. GTK font/scale, dark style, input, idle and rebound keybinding
changes have separate live graphical gates.
