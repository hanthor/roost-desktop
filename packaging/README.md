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

Until the DRM/KMS backend (#52) lands, Roost is nested-only. The preview
image hides the login-screen session entry and installs a
"Roost (nested preview)" launcher that opens Roost in a window inside
GNOME. Upstreaming a `marlin:roost` flavor to `tuna-os/tunaOS` follows
once Roost can start from the login screen.

Try the preview on a Marlin machine:

```sh
sudo bootc switch ghcr.io/hanthor/roost-desktop/marlin-roost-preview:latest
```
