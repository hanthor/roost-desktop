#!/usr/bin/env bash
# Separate genuine GNOME51 session: shared persisted backend, no policy stubs.
set -euo pipefail
fixture_negative=false
if [ "$#" -eq 1 ] && [ "$1" = --fixture-negative ]; then fixture_negative=true
elif [ "$#" -ne 0 ]; then exit 2; fi
if [ "$(id -u)" -eq 0 ]; then
    mkdir -p /run/dbus
    dbus-daemon --system --fork --nopidfile
    chown -R --no-dereference roost-proof:roost-proof /out
    controller=''
    if "$fixture_negative"; then
        python3 /repo/scripts/lib/roost-night-light-fault-controller.py > /out/fault-controller.log 2>&1 & controller=$!
    fi
    status=0
    env -u DBUS_SESSION_BUS_ADDRESS -u ROOST_NIGHT_LIGHT_BUS runuser -u roost-proof -- "$0" "$@" || status=$?
    if [ -n "$controller" ]; then
        kill -TERM "$controller" 2>/dev/null || true
        wait "$controller"
        python3 - <<'MARKER_CLEANUP' || status=1
import json
from pathlib import Path
receipt=json.loads(Path('/out/fault-marker-cleanup.json').read_text())
if receipt.get('failure_type') or (receipt['owned_marker_present'] and not receipt['removed']):
    raise RuntimeError('owned root marker cleanup failed')
MARKER_CLEANUP
    fi
    finalization=0
    python3 /repo/scripts/lib/roost-portal-artifact-finalize.py /out --proof-status "$status" || finalization=$?
    [ "$status" -eq 0 ] || exit "$status"
    exit "$finalization"
fi
if [ -z "${ROOST_NIGHT_LIGHT_BUS:-}" ]; then ROOST_NIGHT_LIGHT_BUS=1 exec dbus-run-session -- "$0" "$@"; fi
[ "$(id -u)" -eq 1000 ]
python3 /repo/scripts/lib/night-light-handoff-tests.py > /out/handoff-policy-tests.txt 2>&1
export DISPLAY=:99 XDG_RUNTIME_DIR=/out/runtime XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state XDG_PICTURES_DIR=/out/pictures
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=Roost:GNOME GSETTINGS_BACKEND=keyfile LIBGL_ALWAYS_SOFTWARE=1 GTK_A11Y=atspi LC_ALL=C.UTF-8 TZ=UTC
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/xdg-desktop-portal" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_PICTURES_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
cp /repo/packaging/marlin/roost-portals.conf "$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf"
id > /out/session-identity.txt
rpm -q gnome-control-center gnome-settings-daemon colord-libs xdg-desktop-portal-gnome gtk4 libadwaita > /out/runtime-versions.txt
for package in gnome-control-center gnome-settings-daemon xdg-desktop-portal-gnome; do
    [ "$(rpm -q --qf '%{VERSION}' "$package" | cut -d. -f1)" = 51 ]
    rpm -V "$package" > "/out/$package-verify.txt"
done
rpm -V colord-libs > /out/colord-libs-verify.txt
rpm -q gtk4-layer-shell python3 python3-gobject > /out/static-source-runtime-versions.txt
rpm -V gtk4-layer-shell > /out/static-layer-library-verify.txt
rpm -qf /usr/lib64/libgtk4-layer-shell.so.0 /usr/bin/python3 > /out/static-source-resource-owners.txt
sha256sum /usr/lib64/libgtk4-layer-shell.so.0 /repo/scripts/lib/roost-night-light-static-client.py > /out/static-source-resource-sha256.txt
rpm -qf /usr/libexec/gsd-color /usr/lib64/libcolord.so.2 > /out/color-resource-owners.txt
python3 - <<'SCHEMAS' > /out/color-schemas.txt
from gi.repository import Gio
source=Gio.SettingsSchemaSource.get_default()
for name in ['org.gnome.settings-daemon.plugins.color','org.gnome.system.location',
             'org.gnome.mutter','org.gnome.desktop.interface','org.gnome.shell.app-switcher',
             'org.gnome.desktop.wm.preferences','org.gnome.desktop.session']:
    schema=source.lookup(name,True)
    if schema is None: raise RuntimeError('actual schema missing: '+name)
    print(schema.get_id(),','.join(sorted(schema.list_keys())))
SCHEMAS
# Normal candidate is an extracted actual Arch artifact, never described as
# installed Fedora packaging. Native shipping qualification is separate.
compositor=/candidate/usr/bin/roost-compositor
if "$fixture_negative"; then compositor=/usr/libexec/roost-vm-night-light-compositor; fi
"$compositor" --version > /out/candidate-version.txt
if ! "$fixture_negative" && grep -q '\[night-light-vm-fixture\]' /out/candidate-version.txt; then exit 1; fi
sha256sum /candidate/usr/bin/roost-compositor /candidate/usr/bin/roost-shell-gtk > /out/candidate-sha256.txt
pids=()
starts=()
register() {
    pids+=("$1")
    starts+=("$(python3 - "$1" <<'START'
from pathlib import Path
import sys
print(Path('/proc/'+sys.argv[1]+'/stat').read_text().rsplit(')',1)[1].split()[19])
START
)")
}
cleanup() {
    local index pid original
    for index in "${!pids[@]}"; do
        pid="${pids[$index]}"; original="${starts[$index]}"
        python3 - "$pid" "$original" <<'CLEANUP' || true
import os,signal,sys,time
from pathlib import Path
pid=int(sys.argv[1]); original=sys.argv[2]
def same():
    try:
        fields=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
        return fields[19]==original and fields[0]!='Z' and Path(f'/proc/{pid}').stat().st_uid==os.getuid()
    except FileNotFoundError: return False
if same():
    os.kill(pid,signal.SIGTERM)
    end=time.monotonic()+2
    while same() and time.monotonic()<end: time.sleep(.05)
    if same(): os.kill(pid,signal.SIGKILL)
CLEANUP
        wait "$pid" 2>/dev/null || true
    done
}
trap cleanup EXIT
Xvfb :99 -screen 0 1280x800x24 -ac -nolisten tcp >/out/xvfb.log 2>&1 & register "$!"
sleep 1
export WAYLAND_DISPLAY=night-light-proof GDK_BACKEND=wayland ROOST_COMPOSITOR_STATE=/out/compositor-state.json
# Scoped to this brand-new bus/session, never the host's activation environment.
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY GDK_BACKEND GTK_A11Y GSETTINGS_BACKEND XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME XDG_PICTURES_DIR LC_ALL TZ
env -u WAYLAND_DISPLAY "$compositor" --backend winit --socket night-light-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/roost-shell-gtk >/out/compositor.log 2>&1 & export ROOST_NIGHT_LIGHT_COMPOSITOR_PID=$!; register "$ROOST_NIGHT_LIGHT_COMPOSITOR_PID"
end=$((SECONDS+30))
until [ -S "$XDG_RUNTIME_DIR/night-light-proof" ] && [ -s "$ROOST_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
# The exact original compositor socket/state exists before color policy starts.
/usr/libexec/gsd-color >/out/daemon.log 2>&1 & export ROOST_NIGHT_LIGHT_DAEMON_PID=$!; register "$ROOST_NIGHT_LIGHT_DAEMON_PID"
python3 /repo/scripts/lib/roost-portal-ready.py org.gnome.SettingsDaemon.Color org.gnome.SettingsDaemon.Color /out/color-ready.json --pid "$ROOST_NIGHT_LIGHT_DAEMON_PID"
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend.log 2>&1 & backend=$!; register "$backend"
python3 /repo/scripts/lib/roost-portal-ready.py org.freedesktop.impl.portal.desktop.gnome org.freedesktop.impl.portal.Screenshot /out/backend-ready.json --pid "$backend"
/usr/libexec/xdg-desktop-portal-gtk >/out/gtk-backend.log 2>&1 & register "$!"
/usr/libexec/xdg-desktop-portal --replace >/out/frontend.log 2>&1 & frontend=$!; register "$frontend"
python3 /repo/scripts/lib/roost-portal-ready.py org.freedesktop.portal.Desktop org.freedesktop.portal.Screenshot /out/frontend-ready.json --pid "$frontend"
python3 /repo/scripts/lib/roost-capture-security-client.py > /out/untrusted-denial.txt
python3 /repo/scripts/lib/roost-night-light-static-client.py > /out/static-client.log 2>&1 & export ROOST_NIGHT_LIGHT_STATIC_PID=$!; register "$ROOST_NIGHT_LIGHT_STATIC_PID"
python3 /repo/scripts/lib/roost-night-light-proof.py "$@" > /out/journey.log 2>&1
rpm -q gnome-control-center gnome-settings-daemon colord-libs xdg-desktop-portal-gnome gtk4 libadwaita > /out/runtime-versions-after.txt
cmp /out/runtime-versions.txt /out/runtime-versions-after.txt
if "$fixture_negative"; then
    printf '%s\n' 'Separate installed feature RPM only: real warm display/raw capture baseline, original controlled optional final-pass injection, neutral fallback/capability false and latched counts. No normal production or physical driver qualification.' > /out/assertions.txt
    exit 0
fi
printf '%s\n' 'GNOME51 genuine color policy and Settings UI, real libcolord Planckian scales, display-only five pixel probes versus genuine untinted Screenshot portal, natural manual clock boundary, original daemon loss/forged-name rejection/recovery, temporary disable API and actual Restart Filter, real Settings restart/persisted keys. Full native/multi-output/shipping and remaining capture/preview phases unqualified; inspect exact runtime receipts.' > /out/assertions.txt
