# Packaging

| Target | Path | Built by |
|---|---|---|
| Debian / Ubuntu `.deb` (Ubuntu 26.04) | `scripts/tuna-release` | CI `journey` job |
| Arch Linux package (TunaOS Marlin) | `packaging/arch/PKGBUILD` | CI `arch-package` job |
| Marlin preview image | `packaging/marlin/Containerfile` | CI `marlin-image` job (main) |

All three stamp the version from the git tag, and every binary's
`--version` must match it; `crates/compositor/tests/release_contract.rs`
pins that for all three.

## TunaOS Marlin

Marlin (`ghcr.io/tuna-os/marlin`) is TunaOS's Arch Linux bootc variant
and Tuna Desktop's first platform. Its GNOME image ships GNOME 51, Tuna Desktop's
parity baseline. The preview image layers the Tuna Desktop package on
`marlin:gnome`, so baseline and candidate share one image family.

The preview image offers "Tuna Desktop (preview)" on the GDM login screen
beside GNOME: the DRM/KMS backend runs the hardware session (#52), the
lock screen unlocks through PAM (`/etc/pam.d/tuna-lock`, #62), and the
power menu logs out back to GDM. A "Tuna Desktop (nested preview)" launcher
also opens Tuna Desktop in a window inside GNOME. Upstreaming a `marlin:tuna`
flavor to `tuna-os/tunaOS` (#69) follows once hardware runs in the VM
lane (#68) are green.

The Arch package and the `.deb` both ship both shells. The compositor
runs the GTK4/libadwaita shell (`tuna-shell-gtk`, ADR 0006) when it is
installed beside it and falls back to the legacy `tuna-shell-host`; set
`TUNA_SHELL_BIN=tuna-shell-host` to choose the legacy shell. The
`.deb` targets Ubuntu 26.04 amd64, whose gtk4-layer-shell meets the 1.1
floor; Debian 13 does not yet (see `docs/install.md`).

`dpkg-deb --root-owner-group` builds the `.deb`, so every archive entry
is `root:root`. CI checks it three ways: `scripts/check-release-package`
(numeric ownership of the original data and control archives, binaries,
versions, runtime floors), `scripts/check-deb-clean-install` (the
`dpkg-deb -c` listing, then an apt install on a pristine `ubuntu:26.04`
container so only the declared `Depends` satisfy the binaries), and the
installed GTK session proof (`scripts/tuna-gtk-shell-proof
--installed-package`).

Try the preview on a Marlin machine:

```sh
sudo bootc switch ghcr.io/tuna-os/tuna-desktop/marlin-tuna-preview:latest
```

## Upgrading from the former name

<!-- tuna-rename: keep-begin -->
Tuna Desktop was called Roost until #505. The package is now
`tuna-desktop` (not `tuna`: Fedora and others ship an unrelated `tuna`),
and both packages declare `replaces`/`conflicts`/`provides=roost`, so an
upgrade swaps it in place. For one release the packages also keep:

- `/usr/bin/roost-*` symlinks to the six `tuna-*` binaries, for scripts
  and greeter configs that name them;
- `/usr/share/wayland-sessions/roost.desktop`
  (`share/compat/wayland-sessions/roost.desktop`): a `NoDisplay=true`
  entry that runs `tuna-session`. GDM remembers each user's last session
  by file name (`roost`); without this entry those users would silently
  land in another desktop after the upgrade. `NoDisplay` keeps it out of
  GDM's and Tuna's session pickers, while GDM still launches it when
  remembered (only `Hidden=true` would stop that). The visible entry is
  `tuna.desktop`;
- `/etc/pam.d/roost-lock` (`share/compat/pam.d/roost-lock`): the lock
  screen asks PAM for its service by name, and a session started before
  an in-place upgrade keeps running the old binary, which asks for
  `roost-lock`. Without the file PAM falls back to `other` (deny on most
  systems) and that session could not be unlocked. New binaries use
  `tuna-lock`; both are conffiles.

The binaries still read `ROOST_*` environment variables as `TUNA_*`
(logging one deprecation line), and adopt per-user state kept under the
old directory names (`~/.config/roost`, `~/.local/share/roost-shell`,
`~/.local/state/roost-shell`, ...) by linking the new name to it
(`crates/shell-control-schema/src/legacy.rs`). All of this goes one
release after the rename.

Other branches rename themselves with `scripts/rename-to-tuna`.
<!-- tuna-rename: keep-end -->
