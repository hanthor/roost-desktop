# Installing Roost

## From the Debian package (recommended on Debian/Ubuntu)

```sh
sudo apt install ./roost_X.Y.Z_amd64.deb
```

Dependencies resolve automatically. After installing, the Roost session is
selectable from the login screen. Upgrading installs the newer package over
the older one with user configuration and application data preserved;
removing the package drops the Roost-owned program files and session entry
while preserving user data. See `docs/release.md` for the release process.

## From source

Prerequisites mirror CI (`check` job plus the display libraries the nested
backend dlopens): a stable Rust toolchain, `libxkbcommon-dev`,
`libxkbcommon-x11-dev`, `libegl1`, `libgl1-mesa-dri`, `libgbm1`,
`pkg-config`.

```sh
cargo build --release \
  -p roost-compositor -p roost-shell-host -p roost-greeter
```

Binaries land in `target/release/`: `roost-compositor`, `roost-shell-host`,
`roost-greeter`, and `roost-session` (the last is a second binary target of
the compositor crate).

## Installing

```sh
PREFIX=/usr/local
install -Dm0755 target/release/roost-compositor "$DESTDIR$PREFIX/bin/roost-compositor"
install -Dm0755 target/release/roost-shell-host "$DESTDIR$PREFIX/bin/roost-shell-host"
install -Dm0755 target/release/roost-greeter   "$DESTDIR$PREFIX/bin/roost-greeter"
install -Dm0755 target/release/roost-session   "$DESTDIR$PREFIX/bin/roost-session"
install -Dm0644 share/wayland-sessions/roost.desktop \
  "$DESTDIR$PREFIX/share/wayland-sessions/roost.desktop"
```

Optional compatibility symlink for configs written against the `rwd`
working title (the greeter still detects a legacy `rwd.desktop` entry,
per `legacy_rwd_entry_still_detected_as_default`):

```sh
ln -s roost.desktop "$DESTDIR$PREFIX/share/wayland-sessions/rwd.desktop"
```

Keep the four binaries side by side: `roost-session` finds its
`roost-compositor` sibling (and the compositor finds `roost-shell-host`) by
directory before falling back to `ROOST_SHELL_BIN` and `PATH`.

## Greetd wiring

Any greetd greeter that lists `wayland-sessions` works. The Roost greeter
enumerates `/usr/share/wayland-sessions` (then `xsessions`), marks the
`Name=Roost` entry default, and launches its `Exec=` line — which must be
`roost-session`, never the compositor directly, so shell supervision stays
in the loop. A minimal `/etc/greetd/config.toml` session:

```toml
[default_session]
command = "roost-greeter"
user = "greeter"
```

(The greeter takes no session-dir flags: it always enumerates the
system directories. The contract that matters is the session file above
parsing to `["roost-session"]`, covered by
`crates/greeter/tests/shipped.rs`.)
