# Screenshot selection keyboard controls

The selection overlay follows GNOME Shell 51.0 `screenshot.js`: arrows resize the selected edge, Alt moves the area, Ctrl adjusts by one logical pixel, Shift reaches the output edge, and R restores the initial area while preserving the selected edge. Both endpoints remain inside the current output, including after an output shrinks. Crossing an endpoint preserves the oriented selection.

Keyboard adjustment relocates the pointer to the selected edge, or to the area centre during Alt movement. Coordinates use integer logical pixels, as Mutter 51.0 `ClutterSeat.warp_pointer`. The private authenticated shell control socket carries the feedback; it adds no public D-Bus API. The compositor rechecks the lock, the mapped exclusive screenshot layer and local bounds before applying it. Existing pointer constraints remain authoritative. Winit also updates the visible host cursor.

## Focused runtime proof

```sh
scripts/tuna-gtk-shell-proof --screenshot-keys-only --display :97 \
  --artifacts /tmp/tuna-screenshot-keys
```

The proof owns a mode-0700 runtime directory, session bus and accessibility bus. It exports the explicit accessibility address to the shell and queries. Each fixture sends real keys through the exclusive GTK overlay, checks the accessible selected area, compositor pointer state and visible host cursor, saves a real PNG, and checks its dimensions. It also checks modified navigation in Screen mode leaves the pointer and workspace unchanged. Source/binary hashes and session identities are retained with the observations. Supplied source binaries can be preserved with `TUNA_PROOF_BIN_DIR`; record their actual build commit with `TUNA_PROOF_BINARY_SOURCE_COMMIT`. Installed-package mode rejects this override.

These fixtures are local nested GTK evidence. They do not qualify a Marlin installed image, native DRM cursor rendering, multiple outputs/scales, a paired GNOME capture, hardware performance or power usage. The complete merged source must still pass the normal CI GTK journey.

## GTK accessibility lifetime

Local GTK 4.14.5, gtk4-layer-shell 1.1.1 and libadwaita 1.5.0 exposed a real screenshot reopen/capture crash. Attached GDB stopped at SIGSEGV in `g_variant_builder_add_value`, reached through GTK's AT-SPI root method while encoding an object reference. The retained diagnostic is `/tmp/tuna-v1-screenshot-attach-backtrace.txt`, SHA-256 `535a2c9795a6307dd61bc7beeeae96a34ce64e3b054cb4e10d9a1816e7356f41`.

[GTK 4.14.5 `gtkatspiroot.c`](https://github.com/GNOME/gtk/blob/4.14.5/gtk/a11y/gtkatspiroot.c) `GetChildAtIndex` can fall through to a hidden final toplevel when a cached index exceeds the new visible count, then encode its null/unrealized accessibility path. The same handling exists in [GTK 4.23.3](https://github.com/GNOME/gtk/blob/4.23.3/gtk/a11y/gtkatspiroot.c), the version declared by the registered RPM package source `src/gnome-51/gtk4/gtk4.spec` (release 3). Marlin's actual Arch GTK version must be recorded from the installed image; the RPM pin does not establish that image's dependency version.

The screenshot UI retires its GtkWindow on close and creates a fresh layer window on reopen, preserving the overlay content and key controller. This removes the retired screenshot toplevel from GTK's accessibility root rather than leaving an unrealized hidden window in its list. The proof also waits for actual layer map/unmap before walking the tree. This application lifecycle correction does not fix GTK's general stale-index behaviour for unrelated windows; unrestricted screen-reader queries during every possible window transition remain a dependency qualification limitation.
