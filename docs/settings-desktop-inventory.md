# GNOME 51 desktop schema inventory

All 429 keys from `gsettings-desktop-schemas-51.0-1.fc45.x86_64` in the GNOME 51 reference container. Source re-audit: main `44edb6aca704ef0ada32b8c6111383ea5c3c6afe` (2026-10-06). See [the control-center audit](gnome-settings-audit.md) for open implementation and qualification gaps. “Via GTK” means the GTK Wayland backend translates the key through its settings portal or direct GSettings fallback; it does not imply support by every application or the compositor. “Ignored” records a current limitation, not permission to disregard binding knowledge.

Handedness follow-up: main `c822ab91abbb8c07fdd89ddde5c81a71f7843ff3`, after qualified #379. Mouse and touchpad dispositions below reflect the merged libinput consumers; tablet handedness remains unsupported.

Source: [GTK Wayland settings translations](https://github.com/GNOME/gtk/blob/4.14.5/gdk/wayland/gdkdisplay-wayland.c). Related shell, Mutter and settings-daemon keys are in [settings-map.md](settings-map.md).

## org.gnome.desktop.a11y

| Key | Status | Reason |
|---|---|---|
| always-show-text-caret | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| always-show-universal-access-status | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
## org.gnome.desktop.a11y.applications

| Key | Status | Reason |
|---|---|---|
| screen-keyboard-enabled | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| screen-magnifier-enabled | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| screen-reader-enabled | Honored (hardware session only) | Hardware GTK shell reconciles the preference with the distribution Orca service after session readiness; nested sessions do not manage host services. Genuine Orca activation/logout VM probes are required; Settings UI activation and complete spoken lock/login/shell navigation remain unqualified (#352) |
## org.gnome.desktop.a11y.interface

| Key | Status | Reason |
|---|---|---|
| high-contrast | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| keyboard-focus-visible-timeout | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| reduced-motion | Honored | GNOME 51 reduce/no-preference enum combines with enable-animations for shell GTK transitions, compositor motion and idle fade; live proof G-ANIMATIONS-OFF; effective Introspect AnimationsEnabled and PropertiesChanged covered by candidate G-INTROSPECT-MOTION (qualification pending) |
| show-status-shapes | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
## org.gnome.desktop.a11y.keyboard

| Key | Status | Reason |
|---|---|---|
| bouncekeys-beep-reject | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| bouncekeys-delay | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| bouncekeys-enable | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| disable-timeout | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| enable | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| feature-state-change-beep | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| mousekeys-accel-time | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| mousekeys-enable | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| mousekeys-init-delay | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| mousekeys-max-speed | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| slowkeys-beep-accept | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| slowkeys-beep-press | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| slowkeys-beep-reject | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| slowkeys-delay | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| slowkeys-enable | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| stickykeys-enable | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| stickykeys-modifier-beep | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| stickykeys-two-key-off | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| timeout-enable | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| togglekeys-enable | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
## org.gnome.desktop.a11y.magnifier

| Key | Status | Reason |
|---|---|---|
| brightness-blue | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| brightness-green | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| brightness-red | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| caret-tracking | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| color-saturation | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| contrast-blue | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| contrast-green | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| contrast-red | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| cross-hairs-clip | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| cross-hairs-color | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| cross-hairs-length | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| cross-hairs-opacity | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| cross-hairs-thickness | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| focus-tracking | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| invert-lightness | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| lens-mode | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| mag-factor | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| mouse-tracking | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| screen-position | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| scroll-at-edges | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| show-cross-hairs | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
## org.gnome.desktop.a11y.mouse

| Key | Status | Reason |
|---|---|---|
| click-type-window-visible | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-click-enabled | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-gesture-double | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-gesture-drag | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-gesture-secondary | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-gesture-single | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-mode | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-threshold | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| dwell-time | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| secondary-click-enabled | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
| secondary-click-time | Ignored | Roost has no equivalent compositor accessibility feature; toolkit support is listed separately |
## org.gnome.desktop.app-folders

| Key | Status | Reason |
|---|---|---|
| folder-children | Honored | Read when app grid rebuilds |
## org.gnome.desktop.app-folders.folder

| Key | Status | Reason |
|---|---|---|
| apps | Honored | Read when app grid rebuilds |
| categories | Honored | Read when app grid rebuilds |
| excluded-apps | Honored | Read when app grid rebuilds |
| name | Honored | Read when app grid rebuilds |
| translate | Honored | Read when app grid rebuilds |
## org.gnome.desktop.background

| Key | Status | Reason |
|---|---|---|
| color-shading-type | Ignored | Roost implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
| picture-opacity | Ignored | Roost implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
| picture-options | Honored (partial) | none hides the picture; every other mode currently uses compositor zoom placement |
| picture-uri | Honored | Live wallpaper URI or solid-color fallback consumed by compositor |
| picture-uri-dark | Honored | Live wallpaper URI or solid-color fallback consumed by compositor |
| primary-color | Honored | Live wallpaper URI or solid-color fallback consumed by compositor |
| secondary-color | Ignored | Roost implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
| show-desktop-icons | Ignored | Roost implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
## org.gnome.desktop.break-reminders

| Key | Status | Reason |
|---|---|---|
| selected-breaks | Ignored | Roost has no break reminder scheduler |
## org.gnome.desktop.break-reminders.eyesight

| Key | Status | Reason |
|---|---|---|
| countdown | Ignored | Roost has no break reminder scheduler |
| delay-seconds | Ignored | Roost has no break reminder scheduler |
| duration-seconds | Ignored | Roost has no break reminder scheduler |
| fade-screen | Ignored | Roost has no break reminder scheduler |
| interval-seconds | Ignored | Roost has no break reminder scheduler |
| lock-screen | Ignored | Roost has no break reminder scheduler |
| notify | Ignored | Roost has no break reminder scheduler |
| notify-overdue | Ignored | Roost has no break reminder scheduler |
| notify-upcoming | Ignored | Roost has no break reminder scheduler |
| play-sound | Ignored | Roost has no break reminder scheduler |
## org.gnome.desktop.break-reminders.movement

| Key | Status | Reason |
|---|---|---|
| countdown | Ignored | Roost has no break reminder scheduler |
| delay-seconds | Ignored | Roost has no break reminder scheduler |
| duration-seconds | Ignored | Roost has no break reminder scheduler |
| fade-screen | Ignored | Roost has no break reminder scheduler |
| interval-seconds | Ignored | Roost has no break reminder scheduler |
| lock-screen | Ignored | Roost has no break reminder scheduler |
| notify | Ignored | Roost has no break reminder scheduler |
| notify-overdue | Ignored | Roost has no break reminder scheduler |
| notify-upcoming | Ignored | Roost has no break reminder scheduler |
| play-sound | Ignored | Roost has no break reminder scheduler |
## org.gnome.desktop.calendar

| Key | Status | Reason |
|---|---|---|
| show-weekdate | Honored | Live ISO week labels use each displayed row's Thursday; GTK gate G-CAL-PREFS |
| week-start-day | Honored | Live first-weekday enum; default and reset follow locale; GTK gate G-CAL-PREFS |
## org.gnome.desktop.datetime

| Key | Status | Reason |
|---|---|---|
| automatic-timezone | Ignored | Time service is external; Roost has no automatic timezone policy |
## org.gnome.desktop.default-applications.office.calendar

| Key | Status | Reason |
|---|---|---|
| exec | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
| needs-term | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
## org.gnome.desktop.default-applications.office.tasks

| Key | Status | Reason |
|---|---|---|
| exec | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
| needs-term | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
## org.gnome.desktop.default-applications.terminal

| Key | Status | Reason |
|---|---|---|
| exec | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
| exec-arg | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
## org.gnome.desktop.input-sources

| Key | Status | Reason |
|---|---|---|
| current | Ignored | Roost uses sources/options and its active input state; this ancillary metadata/policy is not consumed |
| mru-sources | Ignored | Roost uses sources/options and its active input state; this ancillary metadata/policy is not consumed |
| per-window | Ignored | Roost uses sources/options and its active input state; this ancillary metadata/policy is not consumed |
| show-all-sources | Ignored | Roost uses sources/options and its active input state; this ancillary metadata/policy is not consumed |
| sources | Honored | Live compositor XKB keymap; IBus sources use bridge |
| xkb-model | Ignored | Roost uses sources/options and its active input state; this ancillary metadata/policy is not consumed |
| xkb-options | Honored | Live compositor XKB keymap; IBus sources use bridge |
## org.gnome.desktop.interface

| Key | Status | Reason |
|---|---|---|
| accent-color | Honored | Live compositor overview accent and libadwaita style |
| avatar-directories | Ignored | No Roost consumer or verified toolkit translation for this preference |
| can-change-accels | Ignored | No Roost consumer or verified toolkit translation for this preference |
| clock-format | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| clock-show-date | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| clock-show-seconds | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| clock-show-weekday | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| color-scheme | Honored | Live dark tile/background selection; libadwaita apps follow style; shell chrome stays dark |
| cursor-blink | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-blink-time | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-blink-timeout | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-size | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-theme | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| document-font-name | Ignored | No Roost consumer or verified toolkit translation for this preference |
| enable-animations | Honored | Live idle shield fade duration, GTK transitions and compositor overview/strip motion; proofs G-ANIMATIONS-OFF and G-INTROSPECT-MOTION verify effective motion through actual GNOME clients |
| enable-hot-corners | Honored (partial) | Live enable/disable and output-aware LTR corner regions; candidate fullscreen guard has protocol/GTK proofs awaiting execution. Pressure barriers, RTL, retrigger/toggle policy and physical multi-output qualification remain in #336 |
| font-antialiasing | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| font-hinting | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| font-name | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| font-rendering | Ignored | No Roost consumer or verified toolkit translation for this preference |
| font-rgba-order | Ignored (shell) | Shell explicitly uses rgb for rgba antialiasing; GTK clients translate this key independently |
| gtk-color-palette | Ignored | No Roost consumer or verified toolkit translation for this preference |
| gtk-color-scheme | Ignored | No Roost consumer or verified toolkit translation for this preference |
| gtk-enable-primary-paste | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| gtk-im-module | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| gtk-im-preedit-style | Ignored | No Roost consumer or verified toolkit translation for this preference |
| gtk-im-status-style | Ignored | No Roost consumer or verified toolkit translation for this preference |
| gtk-key-theme | Ignored | No Roost consumer or verified toolkit translation for this preference |
| gtk-theme | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| gtk-timeout-initial | Ignored | No Roost consumer or verified toolkit translation for this preference |
| gtk-timeout-repeat | Ignored | No Roost consumer or verified toolkit translation for this preference |
| icon-theme | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| locate-pointer | Ignored | No Roost consumer or verified toolkit translation for this preference |
| menubar-accel | Ignored | No Roost consumer or verified toolkit translation for this preference |
| menubar-detachable | Ignored | No Roost consumer or verified toolkit translation for this preference |
| menus-have-tearoff | Ignored | No Roost consumer or verified toolkit translation for this preference |
| monospace-font-name | Ignored | No Roost consumer or verified toolkit translation for this preference |
| overlay-scrolling | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| scaling-factor | Ignored | Output scale uses Mutter DisplayConfig/monitors.xml, not this XSettings integer |
| show-battery-percentage | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| text-scaling-factor | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| toolbar-detachable | Ignored | No Roost consumer or verified toolkit translation for this preference |
| toolbar-icons-size | Ignored | No Roost consumer or verified toolkit translation for this preference |
| toolbar-style | Ignored | No Roost consumer or verified toolkit translation for this preference |
| toolkit-accessibility | Ignored | No Roost consumer or verified toolkit translation for this preference |
## org.gnome.desktop.lockdown

| Key | Status | Reason |
|---|---|---|
| disable-application-handlers | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-command-line | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-lock-screen | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-log-out | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-print-setup | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-printing | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-save-to-disk | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-show-password | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| disable-user-switching | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| mount-removable-storage-devices-as-read-only | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
| user-administration-disabled | Ignored | Roost has no GNOME lockdown policy consumer; system authorization remains daemon-owned |
## org.gnome.desktop.media-handling

| Key | Status | Reason |
|---|---|---|
| automount | Ignored | Roost has no removable-media autorun/automount agent |
| automount-open | Ignored | Roost has no removable-media autorun/automount agent |
| autorun-never | Ignored | Roost has no removable-media autorun/automount agent |
| autorun-x-content-ignore | Ignored | Roost has no removable-media autorun/automount agent |
| autorun-x-content-open-folder | Ignored | Roost has no removable-media autorun/automount agent |
| autorun-x-content-start-app | Ignored | Roost has no removable-media autorun/automount agent |
## org.gnome.desktop.notifications

| Key | Status | Reason |
|---|---|---|
| application-children | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| show-banners | Honored | Live Do Not Disturb policy |
| show-in-lock-screen | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
## org.gnome.desktop.notifications.application

| Key | Status | Reason |
|---|---|---|
| application-id | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| details-in-lock-screen | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| enable | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| enable-sound-alerts | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| force-expanded | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| show-banners | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| show-in-lock-screen | Ignored | Roost uses global banners and its notification model; per-application/lock-screen preference is not consumed |
## org.gnome.desktop.peripherals.keyboard

| Key | Status | Reason |
|---|---|---|
| delay | Honored | Live compositor seat repeat policy |
| numlock-state | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| remember-numlock-state | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| repeat | Honored | Live compositor seat repeat policy |
| repeat-interval | Honored | Live compositor seat repeat policy |
## org.gnome.desktop.peripherals.mouse

| Key | Status | Reason |
|---|---|---|
| accel-profile | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| custom-accel-config | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| double-click | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| drag-threshold | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| left-handed | Honored | Live GNOME boolean reaches libinput; native VM proof V-MOUSE-HANDEDNESS covers false/true/false on the same compositor |
| middle-click-emulation | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| natural-scroll | Honored | Live libinput policy on hardware devices |
| scroll-wheel-emulation-button | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| scroll-wheel-emulation-button-lock | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| speed | Honored | Live libinput policy on hardware devices |
## org.gnome.desktop.peripherals.pointingstick

| Key | Status | Reason |
|---|---|---|
| accel-profile | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| custom-accel-config | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| scroll-method | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| speed | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.tablet

| Key | Status | Reason |
|---|---|---|
| area | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| keep-aspect | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| left-handed | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| mapping | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| output | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.tablet.deprecated

| Key | Status | Reason |
|---|---|---|
| display | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.tablet.pad-button

| Key | Status | Reason |
|---|---|---|
| action | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| keybinding | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.tablet.stylus

| Key | Status | Reason |
|---|---|---|
| button-action | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| button-keybinding | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-button-action | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-button-keybinding | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-button-mode | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-pressure-curve | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-pressure-range | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| pressure-curve | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| pressure-range | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| secondary-button-action | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| secondary-button-keybinding | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| tertiary-button-action | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| tertiary-button-keybinding | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.touchpad

| Key | Status | Reason |
|---|---|---|
| accel-profile | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| click-method | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| custom-accel-config | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| disable-while-typing | Honored | Live libinput policy, including hotplug |
| disable-while-typing-timeout | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| edge-scrolling-enabled | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| left-handed | Honored | Live left/right/mouse enum reaches libinput, including the hotplug path; physical touchpad and hotplug qualification remain open |
| middle-click-emulation | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| natural-scroll | Honored | Live libinput policy, including hotplug |
| scroll-speed | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| send-events | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| speed | Honored | Live libinput policy, including hotplug |
| tap-and-drag | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| tap-and-drag-lock | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| tap-button-map | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| tap-to-click | Honored | Live libinput policy, including hotplug |
| two-finger-scrolling-enabled | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.touchscreen

| Key | Status | Reason |
|---|---|---|
| orientation-lock | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| output | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.trackball

| Key | Status | Reason |
|---|---|---|
| accel-profile | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| custom-accel-config | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| middle-click-emulation | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| scroll-wheel-emulation-button | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
| scroll-wheel-emulation-button-lock | Ignored | No Roost mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.privacy

| Key | Status | Reason |
|---|---|---|
| disable-camera | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| disable-microphone | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| disable-sound-output | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| hide-identity | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| old-files-age | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| privacy-screen | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| recent-files-max-age | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| remember-app-usage | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| remember-recent-files | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| remove-old-temp-files | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| remove-old-trash-files | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| report-technical-problems | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| send-software-usage-stats | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| show-full-name-in-top-bar | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| usb-protection | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| usb-protection-level | Ignored | Roost has no privacy retention/location/cleanup policy consumer; applications may implement their own |
## org.gnome.desktop.remote-desktop.rdp

| Key | Status | Reason |
|---|---|---|
| auth-methods | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| enable | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| kerberos-keytab | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| screen-share-mode | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| tls-cert | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| tls-key | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| view-only | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
## org.gnome.desktop.remote-desktop.rdp.headless

| Key | Status | Reason |
|---|---|---|
| auth-methods | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| enable | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| kerberos-keytab | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
## org.gnome.desktop.remote-desktop.vnc

| Key | Status | Reason |
|---|---|---|
| auth-method | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| enable | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| encryption | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| screen-share-mode | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| view-only | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
## org.gnome.desktop.remote-desktop.vnc.headless

| Key | Status | Reason |
|---|---|---|
| enable | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Roost does not start or configure that daemon |
## org.gnome.desktop.screen-time-limits

| Key | Status | Reason |
|---|---|---|
| daily-limit-enabled | Ignored | Roost has no screen-time tracking/enforcement service |
| daily-limit-seconds | Ignored | Roost has no screen-time tracking/enforcement service |
| grayscale | Ignored | Roost has no screen-time tracking/enforcement service |
| history-enabled | Ignored | Roost has no screen-time tracking/enforcement service |
## org.gnome.desktop.screensaver

| Key | Status | Reason |
|---|---|---|
| color-shading-type | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| embedded-keyboard-command | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| embedded-keyboard-enabled | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| idle-activation-enabled | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| lock-delay | Honored | Live idle lock policy or lock background |
| lock-enabled | Honored | Live idle lock policy or lock background |
| logout-command | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| logout-delay | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| logout-enabled | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| picture-opacity | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| picture-options | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| picture-uri | Honored | Live idle lock policy or lock background |
| primary-color | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| restart-enabled | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| secondary-color | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| show-full-name-in-top-bar | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| status-message-enabled | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| user-switch-enabled | Ignored | Roost applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
## org.gnome.desktop.search-providers

| Key | Status | Reason |
|---|---|---|
| disable-external | Honored | Provider discovery/order policy |
| disabled | Honored | Provider discovery/order policy |
| enabled | Honored | Provider discovery/order policy |
| sort-order | Honored | Provider discovery/order policy |
## org.gnome.desktop.session

| Key | Status | Reason |
|---|---|---|
| idle-delay | Honored | Live compositor idle policy |
| save-restore | Ignored | Roost session does not consume this legacy session preference |
| session-name | Ignored | Roost session does not consume this legacy session preference |
## org.gnome.desktop.sound

| Key | Status | Reason |
|---|---|---|
| allow-volume-above-100-percent | Ignored | No verified Roost audio preference consumer beyond toolkit translation |
| event-sounds | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| input-feedback-sounds | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| theme-name | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
## org.gnome.desktop.thumbnail-cache

| Key | Status | Reason |
|---|---|---|
| maximum-age | Ignored | Thumbnail cache is external; Roost has no cache retention consumer |
| maximum-size | Ignored | Thumbnail cache is external; Roost has no cache retention consumer |
## org.gnome.desktop.thumbnailers

| Key | Status | Reason |
|---|---|---|
| disable | Ignored | Thumbnail generation is external; Roost has no thumbnailer policy consumer |
| disable-all | Ignored | Thumbnail generation is external; Roost has no thumbnailer policy consumer |
## org.gnome.desktop.wm.keybindings

| Key | Status | Reason |
|---|---|---|
| activate-window-menu | Honored | Live shell accelerator registration and compositor dispatch |
| always-on-top | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| begin-move | Honored | Live shell accelerator registration and compositor dispatch |
| begin-resize | Honored | Live shell accelerator registration and compositor dispatch |
| close | Honored | Live shell accelerator registration and compositor dispatch |
| cycle-group | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| cycle-group-backward | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| cycle-panels | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| cycle-panels-backward | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| cycle-windows | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| cycle-windows-backward | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| lower | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| maximize | Honored | Live shell accelerator registration and compositor dispatch |
| maximize-horizontally | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| maximize-vertically | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| minimize | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-center | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-corner-ne | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-corner-nw | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-corner-se | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-corner-sw | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-monitor-down | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-monitor-left | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-monitor-right | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-monitor-up | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-side-e | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-side-n | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-side-s | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-side-w | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-1 | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-10 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-11 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-12 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-2 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-3 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-4 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-5 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-6 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-7 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-8 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-9 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-down | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| move-to-workspace-last | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-left | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-right | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-up | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| panel-main-menu | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| panel-run-dialog | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| raise | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| raise-or-lower | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| set-spew-mark | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| show-desktop | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-applications | Honored | Live shell accelerator registration and compositor dispatch |
| switch-applications-backward | Honored | Live shell accelerator registration and compositor dispatch |
| switch-group | Honored | Live shell accelerator registration and compositor dispatch |
| switch-group-backward | Honored | Live shell accelerator registration and compositor dispatch |
| switch-input-source | Honored | Live shell accelerator registration and compositor dispatch |
| switch-input-source-backward | Honored | Live shell accelerator registration and compositor dispatch |
| switch-panels | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-panels-backward | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-1 | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-10 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-11 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-12 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-2 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-3 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-4 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-5 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-6 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-7 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-8 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-9 | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-down | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-last | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-left | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-right | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-up | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| switch-windows | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| switch-windows-backward | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| toggle-above | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| toggle-fullscreen | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| toggle-maximized | Honored | Live shell accelerator registration and compositor dispatch |
| toggle-on-all-workspaces | Ignored | This accelerator has no Roost action/registration; only the listed live bindings are grabbed |
| unmaximize | Honored | Live shell accelerator registration and compositor dispatch |
## org.gnome.desktop.wm.preferences

| Key | Status | Reason |
|---|---|---|
| action-double-click-titlebar | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| action-middle-click-titlebar | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| action-right-click-titlebar | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| audible-bell | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| auto-raise | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| auto-raise-delay | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| button-layout | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| disable-workarounds | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| focus-mode | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| focus-new-windows | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| mouse-button-modifier | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| num-workspaces | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| raise-on-click | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| resize-with-right-button | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| theme | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| titlebar-font | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| titlebar-uses-system-font | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| visual-bell | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| visual-bell-type | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
| workspace-names | Ignored | Roost uses click focus and dynamic workspaces; this WM policy has no consumer |
