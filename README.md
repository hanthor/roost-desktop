# Tuna Desktop

Formerly **Roost**. Binaries, packages, crates and environment variables were renamed in [#505](https://github.com/tuna-os/tuna-desktop/issues/505); the old `roost-*` names keep working for one release (see [packaging/README.md](packaging/README.md)).  <!-- tuna-rename: keep -->

A new, independent Wayland desktop session with a GNOME-inspired everyday workflow. This project is not a GNOME Shell rewrite and does not promise compatibility with GNOME Shell extensions or private Mutter APIs.

The compositor is a long-lived Rust process built with Smithay. The shell UI runs as a supervised Wayland client. The first delivery target is a nested developer preview that can map ordinary applications and recover its shell UI after a shell crash.

## Quickstart

- **Install:** [docs/install.md](docs/install.md) covers the Debian-format
  package, building from source and greetd wiring. The experimental TunaOS
  Marlin image is in [packaging/README.md](packaging/README.md).
- **Try it in a window** inside your current GNOME, Wayland or X11 session,
  from a checkout with the [build prerequisites](docs/development.md#prerequisites):

  ```sh
  git clone https://github.com/tuna-os/tuna-desktop.git
  cd tuna-desktop
  ./scripts/tuna-nested run
  ```

  See [the nested session guide](docs/nested-session.md) for options and
  troubleshooting.

See the [walkthrough](docs/walkthrough.md) for every feature as it looks
today, in frames the GTK shell proof takes on each change.

## Screenshots and overview demo

![Tuna Desktop overview with Files, Disks and System Monitor open on the Marlin wallpaper](docs/walkthrough/hero-overview.png)

| App grid | Quick settings |
| --- | --- |
| ![Tuna Desktop app grid with the installed GNOME apps](docs/walkthrough/hero-app-grid.png) | ![Tuna Desktop quick settings with volume, network, Night Light, Dark Style and Do Not Disturb](docs/walkthrough/hero-quick-settings.png) |

| Notifications and calendar | Alt+Tab |
| --- | --- |
| ![Tuna Desktop message list with grouped notifications beside the calendar](docs/walkthrough/hero-notifications.png) | ![Tuna Desktop Alt+Tab switcher with live window thumbnails](docs/walkthrough/hero-switcher.png) |

![Tuna Desktop lock screen clock over the blurred wallpaper](docs/walkthrough/hero-lock.png)

![Overview demo: open the overview with three apps, return to the desktop, then open the app grid](docs/walkthrough/hero-overview.gif)

[Play or download the overview video (MP4)](docs/walkthrough/hero-overview.mp4).
These are frames from the real Marlin image booted in a QEMU VM by CI, on
its hardware (DRM/KMS) session, with the GNOME apps the image ships and its
default wallpaper. See [capture details](docs/walkthrough/README-media.md)
for the run, source revision and how to reproduce.

## Project status

**Nested developer preview.** A Smithay compositor supervises a GTK4/libadwaita
shell with overview window previews, app grid and search, quick settings,
notifications, calendar, keyboard navigation and AT-SPI accessibility. Tuna Desktop
also includes its session supervisor, shell host, greetd greeter and versioned
control protocol. Floating windows, workspaces, Alt-Tab, half tiling, scrollable
tiling and XWayland are covered by graphical CI journeys.

The DRM/KMS backend starts hardware sessions and is tested with virtual KMS
in CI. Physical GPU, VT, suspend and long-running release qualification remain
open. See [the parity ledger](docs/parity-ledger.md) for measured behavior and
remaining gaps.

**Parity target:** GNOME 51. Native GNOME 51 captures provide the visual
reference. The published Marlin image was verified as GNOME Shell 50.5 on
2026-10-04; a GNOME 51 Marlin benchmark image is being validated separately.

**Target platform:** Tuna Desktop is being tested as a TunaOS Marlin flavor (Arch
base, bootc image). The Debian package remains for local development hosts.

**Roadmap:** tracked as GitHub issues on the Tuna Desktop roadmap project board;
[docs/roadmap.md](docs/roadmap.md) keeps the program structure, gates, and
requirement traceability.

## Prerequisites

- **Rust stable** with `rustfmt` and `clippy` (CI uses the current stable
  toolchain; no MSRV is declared, crates use edition 2021).
- **System libraries**: libseat, libinput, libudev, GBM, libdrm,
  libxkbcommon, GTK 4, libadwaita, PipeWire, Wayland and gtk4-layer-shell
  development files, plus EGL (Mesa llvmpipe is enough) to run a nested
  session. The [development guide](docs/development.md#prerequisites)
  has the exact Ubuntu and Arch package lists CI uses.
- **Spektacular CLI** is needed only for spec and plan work, not for
  building or running Tuna Desktop; see the
  [development guide](docs/development.md#spektacular-cli-planning-work-only).

## Contributing and Development

**Want to contribute?** Start with the [development guide](docs/development.md). It covers:
- Setting up your build environment
- Understanding the project structure
- Running a nested Tuna Desktop session locally ([troubleshooting](docs/nested-session.md#troubleshooting))
- [Running tests locally](docs/testing.md), the same checks CI runs
- [Spektacular workflows](docs/development.md#understanding-spektacular-workflows) for planning and tracking

## Documentation

- [Program architecture](docs/architecture.md)
- [Program roadmap and requirement traceability](docs/roadmap.md)
- [Parent requirement register](docs/requirements.md)
- [Verification and test strategy](docs/test-strategy.md)
- [Parity ledger](docs/parity-ledger.md)
- [Research intake rules](docs/research/README.md)
- [Architecture decisions](docs/adr/README.md)
- [Contributing](CONTRIBUTING.md)

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).

The [public roadmap](ROADMAP.md) records priorities through October 2027, current release limits and contribution opportunities.
