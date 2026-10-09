# ADR 0006: The shell is drawn with GTK4 and libadwaita

- Status: Decided (spike landed as `tuna-shell-gtk`; migration tracked per surface)
- Date: 2026-10-01
- Owner: project
- Dependent issues: #53 (decision), #54 overview, #55 panel and quick settings,
  #56 notifications and calendar, #71 accessibility smoke

## Context

Roadmap gate 2 asked for a toolkit spike before lock-in. It never happened:
`tuna-shell-host` grew a hand-written shm painter (8.5k lines in `panel.rs`)
with a 37-glyph, 5-pixel bitmap font. It has no text shaping, no theming, and
no AT-SPI tree, so it cannot look like GNOME 51 and a screen reader cannot use
it. The parity baseline (GNOME 51 in `ghcr.io/tuna-os/marlin:gnome`) is drawn
by GNOME Shell's St toolkit in libadwaita's visual language.

## Decision

Draw the shell with GTK4 + libadwaita, as layer-shell surfaces through
`gtk4-layer-shell`. The shell stays one supervised Wayland client talking to
the compositor only over the versioned control socket (ADR 0004 seam 3).

The spike (`crates/shell-gtk`, binary `tuna-shell-gtk`) draws the GNOME 51
top panel: workspace pill (Activities), centered date and time opening the
calendar plus notification list, and system indicators opening the quick
settings grid. `scripts/tuna-gtk-shell-proof` checks it in the nested
session and through the accessibility bus.

## Evidence (spike, 2026-10-01)

- Text renders in the system interface font (Cantarell here; Adwaita Sans on
  Marlin) with libadwaita styling; frames compare closely with the GNOME 51
  baseline bundle (`baseline-gnome51` release).
- Every panel and quick-settings control is exposed over AT-SPI by name
  (`Activities`, `Date and Time`, `System`, `Wired`, `Dark Style`,
  `Do Not Disturb`, `Volume`, `Lock`); the old painter exposes nothing.
- Dark Style and Do Not Disturb write the same GSettings keys GNOME uses.
- Bringing the spike up found four compositor bugs that broke every GTK4 app,
  all fixed with protocol tests: popups ignored (#88), no `wl_pointer.frame`
  (GTK4 dropped every click), scroll never delivered outside strip mode, and a
  top-strip rule that toggled the overview on any panel click.

## Alternatives considered

- **Keep the custom painter, add cosmic-text and an AccessKit bridge.** Rejected:
  rebuilds a toolkit (text, layout, theming, input methods, accessibility)
  to reach what GTK already provides, and still would not use libadwaita's
  look that GNOME 51 users recognize.
- **A Rust-native toolkit (iced, slint).** Rejected for the shell: neither
  matches libadwaita visually, and screen-reader support is younger than
  GTK's AT-SPI backend.

## Consequences

- New build dependency: `gtk4-layer-shell` (packaged on Arch/Marlin; built
  from source on Ubuntu 24.04 CI).
- Surfaces migrate one at a time while `tuna-shell-host` stays the default
  shell: panel and quick settings (#55), calendar and notifications (#56),
  overview with previews and app grid (#54), dock, banners, lock. The
  toolkit-free cores in `tuna-shell-host` (control client, search, tiles,
  notifications center, model) are reused, not rewritten.
- `TUNA_SHELL_BIN=tuna-shell-gtk` selects the GTK shell today; the default
  flips when it reaches parity with the surfaces the old shell draws.
- The accessibility tree is now testable in CI (#71).
