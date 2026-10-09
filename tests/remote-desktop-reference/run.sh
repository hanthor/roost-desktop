#!/usr/bin/env bash
set -euo pipefail
if [ -z "${TUNA_REMOTE_BUS:-}" ]; then TUNA_REMOTE_BUS=1 exec dbus-run-session -- "$0"; fi
export DISPLAY=:99 XDG_RUNTIME_DIR=/out/runtime XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME GSETTINGS_BACKEND=memory LIBGL_ALWAYS_SOFTWARE=1 GTK_A11Y=atspi
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/xdg-desktop-portal" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
printf '[preferred]\ndefault=gnome;gtk;\norg.freedesktop.impl.portal.ScreenCast=gnome\n' >"$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf"
rpm -q xdg-desktop-portal-gnome xdg-desktop-portal pipewire gtk4 libadwaita libei > /out/runtime-versions.txt
[ "$(rpm -q --qf '%{VERSION}' xdg-desktop-portal-gnome | cut -d. -f1)" = 51 ] || { echo 'GNOME51 backend required' >&2; exit 1; }
# pkg-config deliberately returns separate compiler arguments.
# shellcheck disable=SC2046
gcc -Wall -Wextra -Werror /repo/scripts/lib/tuna-remote-ei.c -o /out/remote-ei $(pkg-config --cflags --libs libei-1.0)
pids=""
cleanup() { for pid in $pids; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT
Xvfb :99 -screen 0 1280x800x24 -ac -nolisten tcp >/out/xvfb.log 2>&1 & pids="$pids $!"
sleep 1
pipewire >/out/pipewire.log 2>&1 & pids="$pids $!"
sleep 1
wireplumber >/out/wireplumber.log 2>&1 & pids="$pids $!"
export WAYLAND_DISPLAY=remote-proof GDK_BACKEND=wayland
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY GDK_BACKEND GTK_A11Y XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME
export TUNA_COMPOSITOR_STATE=/out/compositor-state.json
env -u WAYLAND_DISPLAY /candidate/usr/bin/tuna-compositor --backend winit --socket remote-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/tuna-shell-gtk >/out/compositor.log 2>&1 & pids="$pids $!"
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/remote-proof" ] && [ -s "$TUNA_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
export WAYLAND_DISPLAY=remote-proof GDK_BACKEND=wayland
python3 /repo/scripts/lib/tuna-remote-input-window.py /out/input.json >/out/window.log 2>&1 & pids="$pids $!"
python3 /repo/scripts/lib/tuna-a11y-dump.py tuna-shell-gtk /out/a11y-shell.json 30
sleep 1
python3 /repo/scripts/lib/tuna-remote-security-client.py > /out/untrusted-denial.log
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend.log 2>&1 & portal_backend_pid=$!; pids="$pids $portal_backend_pid"
/usr/libexec/xdg-desktop-portal --replace >/out/frontend.log 2>&1 & pids="$pids $!"
sleep 2
for decision in cancel grant eis disconnect; do
    python3 /repo/scripts/lib/tuna-portal-remote-client.py "/out/$decision.png" "$decision" >"/out/$decision.log" 2>&1 & client=$!; pids="$pids $client"
    python3 /repo/scripts/lib/tuna-portal-consent.py "/out/a11y-$decision.json" "$decision" remote
    wait "$client"
done
[ -s /out/grant.png ] && [ -s /out/disconnect.png ] && [ ! -f /out/cancel.png ]
# Losing the trusted backend process revokes the compositor grant itself.
python3 /repo/scripts/lib/tuna-portal-remote-client.py /out/backend-disconnect.png backend-disconnect >/out/backend-disconnect.log 2>&1 & client=$!; pids="$pids $client"
python3 /repo/scripts/lib/tuna-portal-consent.py /out/a11y-backend-disconnect.json grant remote
end=$((SECONDS + 30))
until [ -s /out/backend-disconnect.active ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
kill "$portal_backend_pid"
touch /out/backend-disconnect.revoke
wait "$client"
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend-restarted.log 2>&1 & pids="$pids $!"
/usr/libexec/xdg-desktop-portal --replace >/out/frontend-restarted.log 2>&1 & pids="$pids $!"
sleep 2
# Keep a genuine external portal stream active across the lock transition.
python3 /repo/scripts/lib/tuna-portal-remote-client.py /out/lock-grant.png revoke >/out/lock-grant.log 2>&1 & client=$!; pids="$pids $client"
python3 /repo/scripts/lib/tuna-portal-consent.py /out/a11y-lock-grant.json grant remote
end=$((SECONDS + 30))
until [ -s /out/lock-grant.active ] && jq -e '.capture_streams > 0 and (.locked | not)' "$TUNA_COMPOSITOR_STATE" >/dev/null; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
gdbus call --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver --method org.gnome.ScreenSaver.SetActive true >/out/lock.log
end=$((SECONDS + 5))
touch /out/lock-grant.revoke
wait "$client"
until jq -e '.locked and .capture_streams == 0' "$TUNA_COMPOSITOR_STATE" >/dev/null; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
python3 /repo/scripts/lib/tuna-remote-security-client.py > /out/locked-denial.log
python3 /repo/scripts/lib/tuna-portal-remote-client.py /out/locked.png locked >/out/locked.log 2>&1 & client=$!; pids="$pids $client"
sleep 2
if [ ! -f /out/locked.response.json ]; then python3 /repo/scripts/lib/tuna-portal-consent.py /out/a11y-locked.json grant remote; fi
wait "$client"
[ ! -f /out/locked.png ]
pw-dump > /out/locked-pipewire.json
jq -e 'all(.[]; .type != "PipeWire:Interface:Node" or (.info.props["node.name"] // "" | contains("tuna-screen-cast") | not))' /out/locked-pipewire.json >/dev/null
printf '%s\n' 'GNOME51 genuine RemoteDesktop portal: Cancel, keyboard/pointer grant, linked PipeWire frame, foreign input denial, legacy GTK delivery, libei GTK delivery/FD revocation, Close/client/backend disconnect, active lock revocation and locked denial; touch/clipboard unsupported' > /out/assertions.txt
