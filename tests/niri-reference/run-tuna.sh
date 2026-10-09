#!/bin/sh
set -eu
export XDG_RUNTIME_DIR=/out/runtime
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
export DISPLAY=:99 GSK_RENDERER=cairo ADW_DISABLE_PORTAL=1 GDK_BACKEND=wayland
export XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state
export GSETTINGS_BACKEND=memory TUNA_COMPOSITOR_STATE=/out/compositor-state.json
export PATH="/candidate/usr/bin:$PATH"
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
tuna-compositor --version >/out/version.txt
rpm -q niri gtk4 libadwaita mesa-dri-drivers >/out/runtime-versions.txt
tuna-compositor --backend winit --width 1280 --height 800 --socket tuna-scroll-compare --shell-bin /candidate/usr/bin/tuna-shell-gtk >/out/tuna.log 2>&1 &
pids="$pids $!"
wid=""
for _ in $(seq 1 60); do
    wid="$(xdotool search --name Smithay 2>/dev/null | head -1 || true)"
    [ -z "$wid" ] || break
    sleep 0.2
done
[ -n "$wid" ]
xdotool windowfocus "$wid"
sleep 2
for name in One Two Three; do
    case "$name" in One) color='#3584e4';; Two) color='#00a040';; Three) color='#a03080';; esac
    WAYLAND_DISPLAY=tuna-scroll-compare python3 /proof-lib/tuna-test-window.py "Scroll $name" "$color" >/out/"$name.log" 2>&1 &
    pids="$pids $!"
    sleep 1
done
for _ in $(seq 1 40); do [ "$(jq '.windows | length' "$TUNA_COMPOSITOR_STATE")" = 3 ] && break; sleep 0.2; done
[ "$(jq '.windows | length' "$TUNA_COMPOSITOR_STATE")" = 3 ]
xdotool key super+shift+t
sleep 1
[ "$(jq -r .session_mode "$TUNA_COMPOSITOR_STATE")" = scroll ]
python3 /proof-lib/tuna-scroll-capture.py tuna-desktop /out
