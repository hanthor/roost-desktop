# Installing Tuna Desktop

> **Backends.** `tuna-compositor --backend auto` (the default) runs as a
> DRM/KMS hardware session when started without a host display, for example
> from a TTY through greetd, and as a nested window otherwise. A hardware
> session needs a seat from logind or seatd. Hardware support is new: it is
> proven on the kernel's virtual KMS device in CI (`scripts/tuna-drm-smoke`)
> and not yet qualified on real GPUs; the cursor is drawn in software and
> client cursor images are not composited yet.

## From the Debian-format package

The package ships the preferred GTK shell (`tuna-shell-gtk`) beside the
compositor, which selects it ahead of the legacy `tuna-shell-host`. The
first installed-package CI target is Ubuntu 26.04 amd64: CI installs the
package on a pristine `ubuntu:26.04` container and runs the installed GTK
session proof against it. Packages must be built against their target
distribution's libraries. The required gtk4-layer-shell version is at least
1.1; [Debian 13 stable provides 1.0.4](https://packages.debian.org/trixie/libgtk4-layer-shell0),
so that target still needs a genuine packaged backport and separate installed
session qualification. [Ubuntu 26.04 provides 1.3.0](https://packages.ubuntu.com/resolute/amd64/libgtk4-layer-shell0).
A package built on Ubuntu does not establish Debian 13 ABI compatibility.

```sh
sudo apt install ./tuna-desktop_X.Y.Z_amd64.deb
```

Dependencies resolve automatically on a target with the declared runtime
packages available. After installing, the Tuna Desktop session is
selectable from the login screen. Upgrading installs the newer package over
the older one with user configuration and application data preserved;
removing the package drops the Tuna Desktop-owned program files and session entry
while preserving user data. See `docs/release.md` for the release process.

## From source

Prerequisites mirror CI (`check` job plus the display libraries the nested
backend dlopens): a stable Rust toolchain, `libxkbcommon-dev`,
`libxkbcommon-x11-dev`, `libegl1`, `libgl1-mesa-dri`, `libgbm1`,
`pkg-config`. The GTK shell also needs `libgtk-4-dev`, `libadwaita-1-dev`
and gtk4-layer-shell (`scripts/ci-install-gtk4-layer-shell` builds it
where the distribution has no package).

```sh
cargo build --release \
  -p tuna-compositor -p tuna-shell-host -p tuna-shell-gtk -p tuna-greeter
```

Binaries land in `target/release/`: `tuna-compositor`, `tuna-shell-gtk`,
`tuna-shell-host`, `tuna-greeter`, and `tuna-session` (the last is a second binary target of
the compositor crate).

## Installing

```sh
PREFIX=/usr/local
install -Dm0755 target/release/tuna-compositor "$DESTDIR$PREFIX/bin/tuna-compositor"
install -Dm0755 target/release/tuna-shell-gtk  "$DESTDIR$PREFIX/bin/tuna-shell-gtk"
install -Dm0755 target/release/tuna-shell-host "$DESTDIR$PREFIX/bin/tuna-shell-host"
install -Dm0755 target/release/tuna-greeter   "$DESTDIR$PREFIX/bin/tuna-greeter"
install -Dm0755 target/release/tuna-session   "$DESTDIR$PREFIX/bin/tuna-session"
install -Dm0644 share/wayland-sessions/tuna.desktop \
  "$DESTDIR$PREFIX/share/wayland-sessions/tuna.desktop"
```

Optional compatibility symlink for configs written against the `rwd`
working title (the greeter still detects a legacy `rwd.desktop` entry,
per `legacy_rwd_entry_still_detected_as_default`):

```sh
ln -s tuna.desktop "$DESTDIR$PREFIX/share/wayland-sessions/rwd.desktop"
```

Keep the binaries side by side: `tuna-session` finds its
`tuna-compositor` sibling by directory. The compositor picks its shell
from `TUNA_SHELL_BIN` when set, then `tuna-shell-gtk` and then
`tuna-shell-host` beside it, then the same two on `PATH`. Set
`TUNA_SHELL_BIN=tuna-shell-host` to run the legacy shell.

## Greetd wiring

Any greetd greeter that lists `wayland-sessions` works. The Tuna Desktop greeter
enumerates `/usr/share/wayland-sessions` (then `xsessions`), marks the
`Name=Tuna Desktop` entry default, and launches its `Exec=` line — which must be
`tuna-session`, never the compositor directly, so shell supervision stays
in the loop. A minimal `/etc/greetd/config.toml` session:

```toml
[default_session]
command = "tuna-greeter"
user = "greeter"
```

(The greeter takes no session-dir flags: it always enumerates the
system directories. The contract that matters is the session file above
parsing to `["tuna-session"]`, covered by
`crates/greeter/tests/shipped.rs`.)
