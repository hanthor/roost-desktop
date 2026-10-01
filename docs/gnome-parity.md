# Pixel parity with GNOME 51

Roost aims to look like GNOME 51 to the pixel, element by element and
state by state. The harness below measures that against GNOME Shell 51.0
itself, not against screenshots or memory.

## Reference: GNOME 51 in a container

`scripts/roost-gnome-reference` builds `tests/gnome-reference/Containerfile`
(Fedora 45, GNOME Shell 51.0) with podman. It runs `gnome-shell --headless`
at 1280x800 with `capture.js` as its automation script, the way GNOME's own
performance tests drive the shell. Roost's proof stubs stand in for
NetworkManager, BlueZ, power-profiles-daemon and logind on a private
system bus. Both sides use the same stubs, so quick settings show the same
services.

For each state the script writes a PNG and a JSON list of every visible
styled actor. Each entry has its style class, name, rectangle, font and
colours, so sizes and spacing can be read off exactly. The run also exports
GNOME 51's default wallpapers (`adwaita-l.jxl`, `adwaita-d.jxl`).

## Roost capture and comparison

`scripts/roost-parity-capture` drives the nested compositor and GTK shell
through the same states with the same settings and wallpaper.
`scripts/lib/roost-parity-compare.py` stacks GNOME, Roost and their
difference for each state, optionally cropped to one element. It prints
the mean channel difference and the share of pixels that are visibly off.

```sh
scripts/roost-gnome-reference
scripts/roost-parity-capture
scripts/lib/roost-parity-compare.py target/gnome-reference target/roost-parity target/parity-compare
scripts/lib/roost-parity-compare.py target/gnome-reference target/roost-parity target/parity-compare 01-desktop:0,0,1280,32
```

## States

| State | What it shows |
| --- | --- |
| 00-startup-overview | GNOME opens the overview at login (reference only) |
| 01-desktop | top bar over the default wallpaper |
| 02-calendar | the date menu |
| 03-quick-settings | quick settings |
| 04-overview-empty | the overview with no windows |
| 05-app-grid | the app grid |
| 06-windows | three windows on the desktop |
| 07-overview-windows | the overview with three windows |
| 08-notification | a notification banner |
| 09-calendar-with-notification | the date menu listing it |

Window contents differ by design: GNOME's capture uses its perf-helper
windows and Roost's uses libadwaita test windows. Compare the shell
around them.
