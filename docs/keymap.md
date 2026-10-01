# Roost keymap

The single reference for every keyboard chord in the session:
shell chords drive the panel, dock, popups, and overview; the
compositor owns window chords. The two sets never overlap — shell
chords stay off Super+{tap,PgUp,PgDn,arrows,R,T} and Alt+{Tab,F4},
pinned by collision tests on both crates.

## Shell: focus movement

The shell keeps one keyboard cursor on a region: the panel strip
(clock, three tiles, hosted indicator cells), the dock slots, an
open popup's rows, or the overview's result list. The focused stop
paints an accent ring.

| Chord | Effect |
|---|---|
| Tab / Shift+Tab | Next / previous stop or row, wrapping |
| Left / Right | Previous / next stop or row |
| Up / Down | Panel to dock and back; rows inside a popup or the overview |
| F6 | Cycle panel and dock regions (skips an empty dock) |
| Super+D | Focus the dock |
| Alt+F1 | Focus the panel |

While the overview is open, movement keys walk the result list;
the region keys still jump the shell underneath.

## Shell: activation and Escape

| Chord | Effect |
|---|---|
| Enter, overview open | Activate the focused result row (top hit from any other region) |
| Enter, overview closed | Activate the focused stop: strip stops toggle their popups, dock slots take a left press (launch, switch, stack), popup rows fire |
| Typing, overview open | Search query (restarts the result cursor) |
| Backspace, overview open | Erase a query character |
| Escape, popup open | Close the popup, restore the parked shell focus |
| Escape, overview open, no popup | Dismiss the overview onto the previously focused window |
| Escape, all closed | No-op |

Opening a popup or the overview parks the live cursor on a return
stack; closing settles back through it, so focus never sticks on a
closed surface. No chord here is rebindable yet; there is no AT-SPI
bridge yet.

## Compositor: windows and session

| Chord | Effect |
|---|---|
| Super tap | Toggle the overview |
| Super+PgUp / Super+PgDn | Switch workspace (plus Shift: move the focused window and follow) |
| Alt+Tab / Shift+Alt+Tab | Step the window switcher; Alt release commits, Escape cancels |
| Super+Up / Super+Down | Maximize / restore the focused window |
| Super+Left / Super+Right | Tile the focused window left / right half |
| Alt+F4 | Ask the focused window to close |
| Super+Shift+T | Flip floating / strip session mode |
| Super+R / Super+Shift+R | Step the focused column through width presets (strip mode) |
