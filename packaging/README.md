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

The preview image offers "Roost (preview)" on the GDM login screen
beside GNOME: the DRM/KMS backend runs the hardware session (#52), the
lock screen unlocks through PAM (`/etc/pam.d/roost-lock`, #62), and the
power menu logs out back to GDM. A "Roost (nested preview)" launcher
also opens Roost in a window inside GNOME. Upstreaming a `marlin:roost`
flavor to `tuna-os/tunaOS` (#69) follows once hardware runs in the VM
lane (#68) are green.

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
