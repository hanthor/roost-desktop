# Window map and destroy effects

The compositor renders window effects from GNOME Shell **51.0**
[`windowManager.js`](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/ui/windowManager.js)
(`_mapWindow`, `_destroyWindow`, `_getAnimationWindowType`).

| Window | Map | Destroy |
| --- | --- | --- |
| Normal, including maximized/fullscreen | 150 ms, ease-out-expo; opacity 0→1, scale (.01,.05)→(1,1), bottom-centred pivot | 150 ms, ease-out-quad; opacity 1→0, scale 1→.8, centred pivot |
| Native transient or X11 dialog | 100 ms, ease-out-quad; opacity 0→1, vertical scale 0→1, centred pivot | 100 ms, ease-out-quad; vertical scale 1→0, centred pivot |

Reduced motion keeps the normal-window fades and drops scale/translation.
The upstream dialog map becomes immediate under reduced motion; dialog
close fades instead of collapsing. Disabling animations settles all effects.
GNOME's slowdown factor changes durations through the shared motion policy.
The overview, an active workspace gesture, and the session lock suspend the
effects. X11 menus, tooltips, docks, utility windows and other auxiliary
roles are excluded. Layer surfaces and xdg popups are not toplevel effects.

## Client lifetime and cost

A shown window retains its last imported Smithay surface render elements.
These own imported GPU textures and buffer references; their drawing method
reads the texture, not the `wl_surface`. The compositor copies those retained
elements into an offscreen texture only after the mapped window disappears.
The closing texture therefore survives a destroyed surface or exited client.
The window leaves the focus/model immediately; a visual ghost cannot receive
input. Closing dialogs vanish when their parent is removed. Hidden windows
are not copied, and switching workspaces does not replay a map effect.

There is no per-frame offscreen copy in steady state. Each visible window
still has one extra surface-tree traversal/import-cache lookup to retain its
latest elements. Measure that cost on the approved Marlin comparison host.
Copies are bounded to 32 simultaneous effects and 64 MiB of pixel storage;
exceeding either budget skips a destroy effect with `snapshot-budget` in the
state diagnostic. Copy failure records `snapshot-failed`. These are explicit
qualification limitations, not passing parity cases.

## Focused proof

```sh
cargo test -p tuna-compositor --lib --no-default-features window_lifecycle
cargo build -p tuna-compositor -p tuna-shell-gtk
dbus-run-session -- scripts/tuna-window-lifecycle-proof \
  --bin-dir target/debug --display :95 --artifacts /tmp/tuna-window-lifecycle
```

The runtime proof owns a private X server, settings and runtime directory.
It freezes the existing compositor animation clock, maps a real GTK native
window and transient, advances the clock through their durations, exits the
client, and checks screenshots for the retained final frame, half-way fade
and absence of stale pixels at completion. It also checks reduced-motion
normal-window transforms and immediate map/destroy with animations off.
The proof copies the supplied binaries into its artifact directory and records
SHA-256 hashes, source revision, dirty status and renderer in `manifest.json`.
Build both binaries from the same checkout before running it.
JSON snapshots and PNGs accompany
`assertions.txt`. `window_lifecycle` in the instrumented compositor state
records running effects, opening transforms and recent outcomes.

This is focused local rendering evidence. It does not prove a paired GNOME
capture, all X11 role types, reduced-motion dialog runtime cases, simultaneous close
ordering, hardware texture import paths, or the v1 performance targets. Those
remain qualification work; the parity ledger must not mark the whole row
passing solely from these unit tests.
