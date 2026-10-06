#!/usr/bin/env bash
set -euo pipefail
if [ -z "${ROOST_PORTAL_BUS:-}" ]; then ROOST_PORTAL_BUS=1 exec dbus-run-session -- "$0"; fi
export DISPLAY=:99 XDG_RUNTIME_DIR=/out/runtime XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state
export XDG_PICTURES_DIR=/out/pictures
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME GSETTINGS_BACKEND=memory LIBGL_ALWAYS_SOFTWARE=1 GTK_A11Y=atspi
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/xdg-desktop-portal" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
cp /repo/packaging/marlin/roost-portals.conf "$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf"
rpm -q xdg-desktop-portal-gnome xdg-desktop-portal nautilus pipewire gtk4 libadwaita > /out/runtime-versions.txt
[ "$(rpm -q --qf '%{VERSION}' xdg-desktop-portal-gnome | cut -d. -f1)" = 51 ] || { echo 'GNOME51 backend required' >&2; exit 1; }
pids=""
cleanup() { for pid in $pids; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT
Xvfb :99 -screen 0 1280x800x24 -ac -nolisten tcp >/out/xvfb.log 2>&1 & pids="$pids $!"
sleep 1
pipewire >/out/pipewire.log 2>&1 & pids="$pids $!"
sleep 1
wireplumber >/out/wireplumber.log 2>&1 & pids="$pids $!"
export WAYLAND_DISPLAY=portal-proof GDK_BACKEND=wayland
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY GDK_BACKEND GTK_A11Y XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME
export ROOST_COMPOSITOR_STATE=/out/compositor-state.json
env -u WAYLAND_DISPLAY /candidate/usr/bin/roost-compositor --backend winit --socket portal-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/roost-shell-gtk >/out/compositor.log 2>&1 & pids="$pids $!"
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/portal-proof" ] && [ -s "$ROOST_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
export WAYLAND_DISPLAY=portal-proof GDK_BACKEND=wayland
python3 /repo/scripts/lib/roost-test-window.py 'Portal Proof' '#3584e4' >/out/window.log 2>&1 & pids="$pids $!"
python3 /repo/scripts/lib/roost-a11y-dump.py roost-shell-gtk /out/a11y-shell.json 30
sleep 1
python3 /repo/scripts/lib/roost-capture-security-client.py > /out/untrusted-denial.log
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend.log 2>&1 & portal_backend_pid=$!; pids="$pids $portal_backend_pid"
python3 /repo/scripts/lib/roost-portal-ready.py org.freedesktop.impl.portal.desktop.gnome org.freedesktop.impl.portal.ScreenCast /out/backend-ready.json --pid "$portal_backend_pid"
/usr/libexec/xdg-desktop-portal-gtk >/out/gtk-backend.log 2>&1 & pids="$pids $!"
/usr/libexec/xdg-desktop-portal --replace >/out/frontend.log 2>&1 & portal_frontend_pid=$!; pids="$pids $portal_frontend_pid"
python3 /repo/scripts/lib/roost-portal-ready.py org.freedesktop.portal.Desktop org.freedesktop.portal.ScreenCast /out/frontend-ready.json --pid "$portal_frontend_pid"
[ "$(rpm -q --qf '%{VERSION}' nautilus | cut -d. -f1)" = 51 ] || { echo 'GNOME51 Nautilus required' >&2; exit 1; }
nautilus --gapplication-service >/out/nautilus.log 2>&1 & pids="$pids $!"
for method in OpenFile SaveFile; do
    for decision in cancel grant; do
        file_out="/out/filechooser-$method-$decision"
        python3 /repo/scripts/lib/roost-portal-filechooser-client.py "$file_out" "$method" "$decision" >"$file_out.log" 2>&1 & file_client=$!; pids="$pids $file_client"
        end=$((SECONDS + 15))
        until [ -s "$file_out.waiting.json" ]; do [ "$SECONDS" -lt "$end" ] || { cat "$file_out.log"; exit 1; }; sleep .1; done
        python3 /repo/scripts/lib/roost-portal-filechooser-ui.py "$file_out" >"$file_out-ui.log" 2>&1
        wait "$file_client"
    done
done
if grep -qE 'Failed to open service channel Wayland connection|Compositor service channel missing' /out/nautilus.log; then
    cat /out/nautilus.log
    echo 'Nautilus fell back after native service-channel failure' >&2
    exit 1
fi
for decision in cancel grant; do
    python3 /repo/scripts/lib/roost-portal-screenshot-client.py "/out/screenshot-$decision.png" "$decision" >"/out/screenshot-$decision.log" 2>&1 & shot_client=$!; pids="$pids $shot_client"
    end=$((SECONDS + 15))
    until [ -s "/out/screenshot-$decision.waiting" ]; do [ "$SECONDS" -lt "$end" ] || { cat "/out/screenshot-$decision.log"; exit 1; }; sleep .1; done
    python3 /repo/scripts/lib/roost-portal-consent.py "/out/a11y-screenshot-$decision.json" "$decision"
    wait "$shot_client"
    if [ "$decision" = cancel ]; then
        # The isolated permission store remembers Deny. Reset only this host
        # fixture entry, through its real API, before a fresh Allow journey.
        python3 - <<'PERMISSIONS'
import json
from pathlib import Path
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
def call(method, args):
    return bus.call_sync("org.freedesktop.impl.portal.PermissionStore", "/org/freedesktop/impl/portal/PermissionStore", "org.freedesktop.impl.portal.PermissionStore", method, args, None, Gio.DBusCallFlags.NONE, 5000, None).unpack()
permissions, data = call("Lookup", GLib.Variant("(ss)", ("screenshot", "screenshot")))
assert permissions.get("") == ["no"], permissions
Path("/out/screenshot-denied-permissions.json").write_text(json.dumps(permissions, indent=2))
call("DeletePermission", GLib.Variant("(sss)", ("screenshot", "screenshot", "")))
permissions, data = call("Lookup", GLib.Variant("(ss)", ("screenshot", "screenshot")))
assert "" not in permissions, permissions
Path("/out/screenshot-fresh-permissions.json").write_text(json.dumps(permissions, indent=2))
PERMISSIONS
    fi
done
[ -s /out/screenshot-grant.png ] && [ ! -f /out/screenshot-cancel.png ]
for decision in cancel grant disconnect; do
    python3 /repo/scripts/lib/roost-portal-capture-client.py "/out/$decision.png" "$decision" >"/out/$decision.log" 2>&1 & client=$!; pids="$pids $client"
    python3 /repo/scripts/lib/roost-portal-consent.py "/out/a11y-$decision.json" "$decision"
    wait "$client"
done
[ -s /out/grant.png ] && [ -s /out/disconnect.png ] && [ ! -f /out/cancel.png ]
# Losing the trusted backend process revokes the compositor grant itself.
python3 /repo/scripts/lib/roost-portal-capture-client.py /out/backend-disconnect.png backend-disconnect >/out/backend-disconnect.log 2>&1 & client=$!; pids="$pids $client"
python3 /repo/scripts/lib/roost-portal-consent.py /out/a11y-backend-disconnect.json grant
end=$((SECONDS + 30))
until [ -s /out/backend-disconnect.active ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
kill "$portal_backend_pid"
touch /out/backend-disconnect.revoke
wait "$client"
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend-restarted.log 2>&1 & portal_backend_pid=$!; pids="$pids $portal_backend_pid"
python3 /repo/scripts/lib/roost-portal-ready.py org.freedesktop.impl.portal.desktop.gnome org.freedesktop.impl.portal.ScreenCast /out/backend-restarted-ready.json --pid "$portal_backend_pid"
/usr/libexec/xdg-desktop-portal --replace >/out/frontend-restarted.log 2>&1 & portal_frontend_pid=$!; pids="$pids $portal_frontend_pid"
python3 /repo/scripts/lib/roost-portal-ready.py org.freedesktop.portal.Desktop org.freedesktop.portal.ScreenCast /out/frontend-restarted-ready.json --pid "$portal_frontend_pid"
# Keep a genuine external portal stream active across the lock transition.
python3 /repo/scripts/lib/roost-portal-capture-client.py /out/lock-grant.png revoke >/out/lock-grant.log 2>&1 & client=$!; pids="$pids $client"
python3 /repo/scripts/lib/roost-portal-consent.py /out/a11y-lock-grant.json grant
end=$((SECONDS + 30))
until [ -s /out/lock-grant.active ] && jq -e '.capture_streams > 0 and (.locked | not)' "$ROOST_COMPOSITOR_STATE" >/dev/null; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
gdbus call --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver --method org.gnome.ScreenSaver.SetActive true >/out/lock.log
end=$((SECONDS + 5))
touch /out/lock-grant.revoke
wait "$client"
until jq -e '.locked and .capture_streams == 0' "$ROOST_COMPOSITOR_STATE" >/dev/null; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
python3 /repo/scripts/lib/roost-capture-security-client.py > /out/locked-denial.log
python3 /repo/scripts/lib/roost-portal-screenshot-client.py /out/screenshot-locked.png locked >/out/screenshot-locked.log 2>&1
python3 /repo/scripts/lib/roost-portal-capture-client.py /out/locked.png locked >/out/locked.log 2>&1 & client=$!; pids="$pids $client"
sleep 2
if [ ! -f /out/locked.response.json ]; then python3 /repo/scripts/lib/roost-portal-consent.py /out/a11y-locked.json grant; fi
wait "$client"
[ ! -f /out/locked.png ]
pw-dump > /out/locked-pipewire.json
jq -e 'all(.[]; .type != "PipeWire:Interface:Node" or (.info.props["node.name"] // "" | contains("roost-screen-cast") | not))' /out/locked-pipewire.json >/dev/null
printf '%s\n' 'GNOME51 genuine portal: Screenshot Access Deny/Allow/readable1280x800PNG and authenticated locked false-result completion with empty URI/unchanged images; untrusted/spoof denial, ScreenCast picker Cancel, Share/FD/frame, foreign owner rejection, Close and client/backend-disconnect/node removal, active external stream revoked on lock, locked denial' > /out/assertions.txt
