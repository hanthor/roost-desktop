# Installing RWD

## From source

Prerequisites mirror CI (`check` job plus the display libraries the nested
backend dlopens): a stable Rust toolchain, `libxkbcommon-dev`,
`libxkbcommon-x11-dev`, `libegl1`, `libgl1-mesa-dri`, `libgbm1`,
`pkg-config`.

```sh
cargo build --release \
  -p rwd-compositor -p rwd-shell-host -p rwd-greeter
```

Binaries land in `target/release/`: `rwd-compositor`, `rwd-shell-host`,
`rwd-greeter`, and `rwd-session` (the last is a second binary target of
the compositor crate).

## Installing

```sh
PREFIX=/usr/local
install -Dm0755 target/release/rwd-compositor "$DESTDIR$PREFIX/bin/rwd-compositor"
install -Dm0755 target/release/rwd-shell-host "$DESTDIR$PREFIX/bin/rwd-shell-host"
install -Dm0755 target/release/rwd-greeter   "$DESTDIR$PREFIX/bin/rwd-greeter"
install -Dm0755 target/release/rwd-session   "$DESTDIR$PREFIX/bin/rwd-session"
install -Dm0644 share/wayland-sessions/rwd.desktop \
  "$DESTDIR$PREFIX/share/wayland-sessions/rwd.desktop"
```

Keep the four binaries side by side: `rwd-session` finds its
`rwd-compositor` sibling (and the compositor finds `rwd-shell-host`) by
directory before falling back to `RWD_SHELL_BIN` and `PATH`.

## Greetd wiring

Any greetd greeter that lists `wayland-sessions` works. The RWD greeter
enumerates `/usr/share/wayland-sessions` (then `xsessions`), marks the
`Name=RWD` entry default, and launches its `Exec=` line — which must be
`rwd-session`, never the compositor directly, so shell supervision stays
in the loop. A minimal `/etc/greetd/config.toml` session:

```toml
[default_session]
command = "rwd-greeter"
user = "greeter"
```

(The greeter takes no session-dir flags: it always enumerates the
system directories. The contract that matters is the session file above
parsing to `["rwd-session"]`, covered by
`crates/greeter/tests/shipped.rs`.)
