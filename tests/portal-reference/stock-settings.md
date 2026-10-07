The portal-security job also runs an independent stock GNOME 51 Settings session
against the packaged compositor and GTK shell. Its private session/system buses,
ordinary UID 1000, Wayland socket and shared keyfile settings backend are fresh;
it never inherits the portal/security session's memory backend or locked state.

The Multitasking Hot Corner journey performs the actual accessible switch action,
then reads the persisted setting in a separate process and probes the compositor
with real nested-host pointer input. It checks disabled rejection, enabled overview
opening, Escape dismissal, disabling again, genuine Settings process restart with
the persisted disabled switch, and re-enabling. The test never sets the target key
through CLI or library calls. It pins each application's D-Bus owner, PID, UID,
start time and installed executable hash, matches its accessible process and checks
actual compositor focus. Package versions/verification, accessibility trees, keyfile
snapshots, compositor scenes, screenshots and logs remain in the stock-settings
artifact subdirectory, including on failure.

This qualifies this control's UI-to-key-to-compositor path only. Native edge
pressure, RTL/multiple outputs, compositor/session restart persistence, other
Settings controls, privileged system services and the final shipped image still
need their own runtime evidence. The broader GNOME Settings and hot-corner issues
remain open.

Primary control reference:
https://github.com/GNOME/gnome-control-center/blob/51.0/panels/multitasking/cc-multitasking-panel.c
and its cc-multitasking-panel.blp template. The application ID comes from GNOME's
51.0 meson.build; the keyfile location/monitor semantics come from GLib's
GKeyfileSettingsBackend implementation.
