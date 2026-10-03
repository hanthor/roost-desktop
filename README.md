# Roost

A new, independent Wayland desktop session with a GNOME-inspired everyday workflow. This project is not a GNOME Shell rewrite and does not promise compatibility with GNOME Shell extensions or private Mutter APIs.

The compositor is a long-lived Rust process built with Smithay. The shell UI runs as a supervised Wayland client. The first delivery target is a nested developer preview that can map ordinary applications and recover its shell UI after a shell crash.

See the [walkthrough](docs/walkthrough.md) for every feature as it looks
today, in frames the GTK shell proof takes on each change.

## Project status

**Nested developer preview.** Four crates build and ship: a Smithay compositor
(floating windows, workspaces, Alt-Tab, tiling halves, a scrollable strip mode,
multi-output with hotplug migration, session lock, supervised shell restart,
optional XWayland), a shell host (panel, quick-settings tiles, tray,
notifications daemon, dock, overview search and launch, keyboard navigation,
Rhai extensions, GSettings read and write-back), a greetd greeter, and the
versioned control protocol between compositor and shell.

What it is not yet:

- **Hardware sessions are new.** The DRM/KMS backend (libseat, GBM/EGL,
  libinput) starts from a TTY and is proven on a virtual KMS device in CI,
  but is not yet qualified on real GPUs. Nested runs inside another session
  remain the main development path.
- **Not visually at parity.** The shell paints its own pixels with a small
  bitmap font; there is no toolkit, no app grid, no window previews, and no
  AT-SPI bridge yet.

**Parity baseline:** GNOME 51 as shipped in the TunaOS Marlin GNOME image
(`ghcr.io/tuna-os/marlin:gnome`). Every parity claim is measured against
that image on the same virtual machine; see
[the parity ledger](docs/parity-ledger.md).

**Target platform:** Roost is being tested as a TunaOS Marlin flavor (Arch
base, bootc image). The Debian package remains for local development hosts.

**Roadmap:** tracked as GitHub issues on the Roost roadmap project board;
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
- **Spektacular CLI**, only for spec and plan work, not for building or
  running Roost:
  `go install github.com/hivecommons/spektacular@latest`, then
  `spektacular version check` (see [Continue with Spektacular](#continue-with-spektacular)).

## Contributing and Development

**Want to contribute?** Start with the [development guide](docs/development.md). It covers:
- Setting up your build environment
- Understanding the project structure
- Running a nested Roost session locally ([troubleshooting](docs/nested-session.md#troubleshooting))
- [Running tests locally](docs/testing.md), the same checks CI runs
- Spektacular workflows for planning and tracking

## Planning

- [Program architecture](docs/architecture.md)
- [Program roadmap and requirement traceability](docs/roadmap.md)
- [Parent requirement register](docs/requirements.md)
- [Verification and test strategy](docs/test-strategy.md)
- [Research intake rules](docs/research/README.md)
- [First spek: nested compositor and shell recovery](.spektacular/specs/20260927170317-a01f0011-001-nested-compositor-shell-recovery.md)
- [Architecture decisions](docs/adr/README.md)
- [Contributing](CONTRIBUTING.md)

Spektacular is initialized for Codex. Specs and plans are managed through its CLI; each program unit has a spek plus draft plan, context, and research artifacts. Draft plans must be reviewed against current implementation and open decision gates before their implementation workflow starts.

## Continue with Spektacular

Install the CLI if needed (`go install github.com/hivecommons/spektacular@latest`), then ensure `$(go env GOPATH)/bin` is on `PATH`.

Use the spek's full timestamp-prefixed name from `spektacular spec file list`:

```sh
spektacular version check
spektacular plan new --data '{"name":"20260927170317-a01f0011-001-nested-compositor-shell-recovery"}'
```

The plan workflow should refresh its draft from the current source and the linked spek. Complete its walkthrough and review before starting implementation. The implementation workflow starts with the corresponding full plan name after approval. See [Spektacular](https://github.com/hivecommons/spektacular) for the current workflow and CLI details.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
