# Packaging

| Target | Path | Built by |
|---|---|---|
| Debian / Ubuntu `.deb` (dev hosts) | `scripts/roost-release` | CI `journey` job |
| Arch Linux package (TunaOS Marlin) | `packaging/arch/PKGBUILD` | CI `arch-package` job |
| Marlin preview image | `packaging/marlin/Containerfile` | CI `marlin-image` job (main) |

All three stamp the version from the git tag, and every binary's
`--version` must match it; `crates/compositor/tests/release_contract.rs`
pins that for all three.

## TunaOS Marlin

Marlin (`ghcr.io/tuna-os/marlin`) is TunaOS's Arch Linux bootc variant
and Roost's first platform. Its GNOME image ships GNOME 51, Roost's
parity baseline. The preview image layers the Roost package on
`marlin:gnome`, so baseline and candidate share one image family.

The preview image hides the login-screen session entry and installs a
"Roost (nested preview)" launcher that opens Roost in a window inside
GNOME. The DRM/KMS backend has landed (#52), but the login session stays
hidden until the lock screen checks passwords through PAM (the 004
release gate). Upstreaming a `marlin:roost` flavor to `tuna-os/tunaOS`
follows once that gate passes.

The Arch package ships both shells. The compositor runs the
GTK4/libadwaita shell (`roost-shell-gtk`, ADR 0006) when it is installed
beside it and falls back to the legacy `roost-shell-host`; set
`ROOST_SHELL_BIN=roost-shell-host` to choose the legacy shell. The
`.deb` keeps only the legacy shell, because Ubuntu 24.04 has no
gtk4-layer-shell package.

Try the preview on a Marlin machine:

```sh
sudo bootc switch ghcr.io/hanthor/roost-desktop/marlin-roost-preview:latest
```
