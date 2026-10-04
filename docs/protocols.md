# Wayland protocols

What Roost advertises to clients, against what GNOME 51's Mutter
advertises. Ledger row P-SY-06 tracks the gaps.

**Source of the Mutter column.** A `wayland-info` capture of GNOME
Shell 51.0 / Mutter 51.0 (Fedora 45, headless, 1280x800):
`tests/protocols/gnome51-wayland-info.txt`, made by
`scripts/roost-gnome-wayland-info`. Three globals do not appear headless
(the DRM lease and syncobj managers need the native KMS backend; the
Xwayland keyboard grab is offered to Xwayland only). They are listed from
the interfaces built into the same `libmutter-51.so`, marked `source` in
`tests/protocols/gnome51-globals.tsv`. This is upstream GNOME 51 in a
Fedora container, not a capture from the Marlin baseline VM.

**Comparison.** The proof stage G-WAYLAND-INFO runs `wayland-info`
against the nested Roost session (saved as the `wayland-info.txt`
artifact) and `scripts/roost-wayland-info-compare` checks it against
`tests/protocols/gnome51-globals.tsv`. Every global marked `match` must
be present at GNOME's version or newer, and every `min:` global at least
at the version given. Known gaps are reported and do not fail the stage.

**Golden test.** `crates/compositor/tests/protocols.rs` asserts the exact
list and versions below. Adding or dropping a protocol updates the test
and this page together.

## Advertised by Roost

| Global | Version | Notes |
|---|---|---|
| wl_compositor | 6 | preferred buffer scale sent per surface, for the output it is on |
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
| ext_session_lock_manager_v1 | 1 | the shell's lock screen; only the supervised shell's lock is granted, and only a password the compositor verified unlocks |
| wp_presentation | 2 | CLOCK_MONOTONIC, as Mutter; see frame timing below |
| wp_fifo_manager_v1 | 1 | a barrier is released on the next refresh cycle |
| wp_commit_timing_manager_v1 | 1 | a timed update applies on the refresh cycle whose next presentation reaches its time |
| xdg_wm_dialog_v1 | 1 | a modal dialog is attached to its parent: focusing the parent focuses the dialog (GNOME's attach-modal-dialogs) |
| zxdg_exporter_v2, zxdg_importer_v2 | 1 | xdg-foreign: the portal parents its dialogs to the app's window, which centres and attaches them like the app's own |
| xdg_system_bell_v1 | 1 | plays GNOME's `bell-window-system` sound through `canberra-gtk-play` when installed (Mutter's audible bell); a burst of rings is one sound |
| xdg_toplevel_tag_manager_v1 | 1 | each window keeps its tag and description |
| zwp_keyboard_shortcuts_inhibit_manager_v1 | 1 | after Allow, the focused window gets every key, the shell's grabs, Super and Alt+Tab included; Super+Escape (Mutter's restore-shortcuts) takes them back until the window is focused again |
| wp_pointer_warp_v1 | 1 | honoured while the surface has pointer focus from the enter serial it names; nested, the host pointer stays where it is |

### xdg-activation policy

A token is valid only when the client that asks for it holds keyboard
focus and attaches an input serial. It works once, within 10 seconds.
A valid request focuses and raises the window. This is Mutter's
focus-stealing prevention in its simplest form: a background app cannot
mint its way to the front.

### Frame timing

presentation-time, fifo and commit-timing follow the frame path
(`crates/compositor/src/frame_timing.rs`):

- Feedback is taken from the surfaces a frame drew. On hardware it is
  marked presented at that output's page flip with the kernel's vblank
  time and sequence and Mutter's KMS flags (vsync, hardware clock,
  hardware completion). Nested, it is marked when the host takes the
  frame, timed by the compositor's clock, with no flags (Mutter's nested
  backends cannot vouch for the host either). A surface not drawn (a
  hidden workspace, the lock screen up) keeps its feedback until it is,
  or until a newer update discards it.
- Fifo barriers and commit timers advance once per refresh cycle (the
  primary output's page flip; nested, each frame) on every surface that
  receives frame callbacks, drawn or not, so a client on a hidden
  workspace never stalls.

### Keyboard shortcuts inhibit

Roost asks before letting a focused, mapped app take shortcuts. The GTK
shell shows a modal with Deny and Allow and explains Super+Escape.
Only Allow activates the inhibitor; Deny cannot be undone by refocusing.
Known desktop apps use GNOME's PermissionStore (`gnome` table,
`shortcuts-inhibitor` id, `GRANTED`/`DENIED` values), so remembered grants
skip the dialog. Unknown apps are never remembered. An unavailable store
opens the dialog. Background requests, stale responses, destroyed surfaces
and requests outstanding at lock or a switch to another app fail closed.
The non-GTK fallback shell has no consent UI and keeps requests inactive.
Super+Escape always takes granted shortcuts back until the window is
focused again. GNOME's configurable Xwayland exemption rules are not
implemented. `scripts/roost-shortcut-proof` exercises Allow, Deny, refocus,
emergency restore, stale focus and lock through a real Wayland test client.

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
| org.freedesktop.PolicyKit1.AuthenticationAgent | BeginAuthentication, CancelAuthentication | the GTK shell registers as this session's polkit agent (system bus, again whenever polkit restarts) and shows GNOME's Authentication Required dialog; passwords go through polkit's own polkit-agent-helper-1 |
| org.gnome.Mutter.IdleMonitor | GetIdletime, AddIdleWatch, AddUserActiveWatch, RemoveWatch, WatchFired | the session's idle time and watches, served by the compositor; gnome-settings-daemon dims and blanks on it, and banners wait for an away user |
| org.gnome.Shell | ShowOSD, FocusSearch, ShowApplications, GrabAccelerator(s), UngrabAccelerator(s), AcceleratorActivated; Mode, OverviewActive, ShellVersion | served by the GTK shell: gnome-settings-daemon's media keys grab their keys here (the compositor keeps them from apps, in the action modes they ask for, the lock screen included) and show the volume and brightness OSD; GNOME's caller allowlist (Settings, media keys, the GNOME portal backend); a caller's grabs go when it leaves the bus |

## Gaps against Mutter

From the GNOME 51 capture. The globals not listed here are offered at
GNOME's version.

| Protocol | GNOME 51 | Roost | Why, and where tracked |
|---|---|---|---|
| xdg_wm_base | 7 | 6 | Smithay 0.7 creates the global at 6 and implements nothing newer; bumps with Smithay (#89) |
| wl_seat | 10 | 9 | Smithay 0.7 creates the seat at 9 and implements nothing newer; bumps with Smithay (#89) |
| zwp_text_input_manager_v3 | 2 | 1 | Smithay 0.7 implements v1 only; bumps with Smithay (#60) |
| zwp_tablet_manager_v2 | 2 | missing | Smithay 0.7 implements manager v1 only, and tablet events from libinput are not routed yet (the input path carries pointer, keys and swipes); needs the hardware lane (#68, #89) |
| zxdg_exporter_v1, zxdg_importer_v1 | 1 | missing | GTK3 exports with v1; Smithay 0.7 implements v2 only and keeps its handle table private, so a v1 handle could not be imported by the portal's v2 (#61) |
| xdg_toplevel_drag_manager_v1 | 1 | missing | not in Smithay 0.7: a window attached to a drag-and-drop operation and moved with it (detachable browser tabs) (#89) |
| xdg_session_manager_v1 | 1 | missing | not in Smithay 0.7: window session save and restore is a subsystem of its own (Mutter 51's session management) (#89) |
| wp_color_manager_v1 (v2), wp_color_representation_manager_v1 | 2, 1 | missing | not in Smithay 0.7: needs a colour-managed renderer (ICC/HDR, YUV conversion); the GLES path is sRGB only (#89) |
| ext_background_effect_manager_v1 | 1 | missing | not in Smithay 0.7: needs a blur pass in the renderer (#89) |
| wl_fixes | 1 | missing | not in wayland-server 0.31; its `destroy_registry` frees a registry object, which wayland-backend owns (#89) |
| wp_linux_drm_syncobj_manager_v1, wp_drm_lease_device_v1 (native backend; from source) | — | missing | hardware only: explicit sync needs the DRM backend's syncobj import and the lease needs connectors to hand out (VR headsets); not testable nested (#68, #89) |
| zwp_xwayland_keyboard_grab_manager_v1 (Xwayland only; from source) | — | missing | only rootful Xwayland with `-host-grab` uses it, and Mutter allows grabs only per `xwayland-grab-access-rules`; Roost runs rootless Xwayland (#89) |
| gtk_shell1 | 7 | missing | not planned: GTK4 needs none of it on a GNOME session |
| zwp_linux_dmabuf_v1 feedback (v4+) | native backend only | version 3 | #89 |

## Deliberate differences

- **xdg-decoration is absent,** as in Mutter. GNOME is client-side
  decorations only, and the golden test asserts the absence.
- **layer-shell is present,** unlike Mutter. Roost's shell is a separate
  process and draws its panel, overview and banners through it.
