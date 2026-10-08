#!/bin/sh
set -eu
export XDG_RUNTIME_DIR=/out/runtime
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export DISPLAY=:99 GSK_RENDERER=cairo ADW_DISABLE_PORTAL=1
unset WAYLAND_DISPLAY
pids=""
cleanup() {
    # shellcheck disable=SC2086
    [ -z "$pids" ] || kill $pids 2>/dev/null || true
}
trap cleanup EXIT INT TERM
Xvfb "$DISPLAY" -screen 0 1280x800x24 >/out/xvfb.log 2>&1 &
pids="$!"
for _ in $(seq 1 30); do xdotool getdisplaygeometry >/dev/null 2>&1 && break; sleep 0.2; done
niri validate --config /nref/config.kdl >/out/config-validation.log 2>&1
niri --config /nref/config.kdl >/out/niri.log 2>&1 &
pids="$pids $!"
for _ in $(seq 1 60); do
    for socket in "$XDG_RUNTIME_DIR"/niri.*.sock; do [ ! -S "$socket" ] || export NIRI_SOCKET="$socket"; done
    [ -z "${NIRI_SOCKET:-}" ] || break
    sleep 0.2
done
[ -n "${NIRI_SOCKET:-}" ]
niri --version >/out/version.txt
rpm -q niri gtk4 libadwaita mesa-dri-drivers >/out/runtime-versions.txt
niri msg -j outputs >/out/outputs.json
wid="$(xdotool search --name '^niri$' | head -1)"
xdotool windowsize "$wid" 1280 800
xdotool windowfocus "$wid"
for name in One Two Three; do
    case "$name" in One) color='#3584e4';; Two) color='#00a040';; Three) color='#a03080';; esac
    niri msg action spawn -- python3 /proof-lib/tuna-test-window.py "Scroll $name" "$color"
    sleep 1
done
for _ in $(seq 1 30); do [ "$(niri msg -j windows | jq length)" = 3 ] && break; sleep 0.2; done
[ "$(niri msg -j windows | jq length)" = 3 ]
sleep 1
python3 /proof-lib/tuna-scroll-capture.py niri /out
niri msg -j outputs >/out/outputs.json
