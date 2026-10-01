# Wayland protocols

What Roost advertises to clients, against what GNOME 51's Mutter
advertises. Ledger row P-SY-06 tracks the gaps.

**Source of the Mutter column.** It is read from Mutter's Wayland setup
code, not captured from the Marlin baseline VM. Capturing it with
`wayland-info` inside the baseline VM is the open part of #89; until
then treat the column as a reading, not evidence.

**Golden test.** `crates/compositor/tests/protocols.rs` asserts the exact
list and versions below. Adding or dropping a protocol updates the test
and this page together.

## Advertised by Roost

| Global | Version | Notes |
|---|---|---|
| wl_compositor | 5 | |
| wl_subcompositor | 1 | |
| wl_shm | 2 | |
| wl_seat | 9 | |
| wl_output | 4 | one per connected output |
| wl_data_device_manager | 3 | |
| zwp_primary_selection_device_manager_v1 | 1 | |
| xdg_wm_base | 6 | popups with positioner constraints and grabs |
| zxdg_output_manager_v1 | 3 | |
| zwlr_layer_shell_v1 | 4 | Roost's own shell draws through it |
| zwp_linux_dmabuf_v1 | 3 | only when the renderer reports formats |
| xdg_activation_v1 | 1 | focus policy below |
| wp_viewporter | 1 | |
| wp_fractional_scale_manager_v1 | 1 | preferred scale 1 until #59 |
| wp_single_pixel_buffer_manager_v1 | 1 | |
| wp_cursor_shape_manager_v1 | 2 | shapes accepted; the compositor cursor is still the default arrow |
| zwp_idle_inhibit_manager_v1 | 1 | a live inhibitor holds off the idle lock |
| zwp_text_input_manager_v3 | 1 | text fields reach the input method (#60) |
| zwp_input_method_manager_v2 | 1 | IMEs such as fcitx5; candidate popups are tracked like any popup |
| zwp_pointer_gestures_v1 | 3 | swipes reach apps; three-finger swipes are the shell's (overview, workspaces) |
| zwp_relative_pointer_manager_v1 | 1 | raw motion from the DRM backend |
| zwp_pointer_constraints_v1 | 1 | a locked pointer stays put; a confined one stays in its window |

### xdg-activation policy

A token is valid only when the client that asks for it holds keyboard
focus and attaches an input serial. It works once, within 10 seconds.
A valid request focuses and raises the window. This is Mutter's
focus-stealing prevention in its simplest form: a background app cannot
mint its way to the front.

## GNOME D-Bus interfaces

GNOME's portal backend (xdg-desktop-portal-gnome) talks to GNOME Shell
and Mutter over D-Bus, not Wayland. Roost serves those interfaces itself,
as niri does, so the stock portal works.

| Interface | Roost | Notes |
|---|---|---|
| org.gnome.Shell.Screenshot | Screenshot, ScreenshotWindow | saves a PNG of the screen or the focused window; the name is left to a real GNOME Shell when one runs (nested preview) |
| org.gnome.Mutter.ScreenCast | CreateSession, RecordMonitor, RecordWindow, Start, Stop | monitor and window streams over PipeWire (BGRx, shared memory, up to 30 fps), adapted from niri; window streams follow resizes and end when the window closes |
| org.gnome.Mutter.DisplayConfig | GetCurrentState, ApplyMonitorsConfig | the portal's monitor picker and GNOME Settings' Displays panel (scale and position; no mirroring or rotation yet) |
| org.gnome.Shell.Introspect | yes | window list for the portal's window picker; desktop-portal callers only, like GNOME Shell (#61) |

## Gaps against Mutter

| Protocol | Roost | Tracked in |
|---|---|---|
| wp_presentation | missing | #89 |
| zwp_tablet_manager_v2 | missing | #89 |
| zwp_keyboard_shortcuts_inhibit_manager_v1 | missing | #89 |
| zxdg_exporter_v2, zxdg_importer_v2 (xdg-foreign) | missing | #61 (portal dialogs) |
| wp_linux_drm_syncobj_manager_v1 | missing | #89 |
| wp_drm_lease_device_v1 | missing | #89 (VR headsets) |
| wp_color_manager_v1 | missing | #89 |
| xdg_toplevel_drag_v1, xdg_dialog_v1 | missing | #89 |
| gtk_shell1 | missing | not planned: GTK4 needs none of it on a GNOME session |
| zwp_linux_dmabuf_v1 version 4 and 5 feedback | version 3 only | #89 |

## Deliberate differences

- **xdg-decoration is absent,** as in Mutter. GNOME is client-side
  decorations only, and the golden test asserts the absence.
- **layer-shell is present,** unlike Mutter. Roost's shell is a separate
  process and draws its panel, overview and banners through it.
