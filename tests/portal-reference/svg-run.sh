#!/usr/bin/env bash
set -euo pipefail
if [ "$(id -u)" -eq 0 ]; then
    # A real isolated system bus, without fabricated service implementations.
    mkdir -p /run/dbus
    dbus-daemon --system --fork --nopidfile
    chown -R --no-dereference roost-proof:roost-proof /out
    status=0
    env -u DBUS_SESSION_BUS_ADDRESS -u ROOST_SVG_BUS runuser -u roost-proof -- "$0" || status=$?
    chown -R --no-dereference 0:0 /out
    exit "$status"
fi
if [ -z "${ROOST_SVG_BUS:-}" ]; then ROOST_SVG_BUS=1 exec dbus-run-session -- "$0"; fi
[ "$(id -u)" -eq 1000 ]
export DISPLAY=:99 XDG_RUNTIME_DIR=/out/runtime XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME GSETTINGS_BACKEND=keyfile LIBGL_ALWAYS_SOFTWARE=1 GTK_A11Y=atspi LC_ALL=C.UTF-8
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
id > /out/session-identity.txt
rpm -q gnome-control-center mutter-common gnome-shell-common gnome-desktop4 gsettings-desktop-schemas gtk4 libadwaita at-spi2-core > /out/runtime-versions.txt
[ "$(rpm -q --qf '%{VERSION}' gnome-control-center | cut -d. -f1)" = 51 ]
rpm -V gnome-control-center > /out/settings-package-verify.txt
[ "$(rpm -q --qf '%{VERSION}' mutter-common | cut -d. -f1)" = 51 ]
[ "$(rpm -q --qf '%{VERSION}' gnome-desktop4 | cut -d. -f1)" = 51 ]
rpm -qf /usr/lib64/girepository-1.0/GnomeBG-4.0.typelib > /out/slideshow-reference-owner.txt
rpm -V gnome-desktop4 > /out/slideshow-reference-verify.txt
rpm -qf /usr/share/glib-2.0/schemas/org.gnome.mutter.gschema.xml > /out/mutter-schema-owner.txt
rpm -V mutter-common > /out/mutter-package-verify.txt
[ "$(rpm -q --qf '%{VERSION}' gnome-shell-common | cut -d. -f1)" = 51 ]
rpm -qf /usr/share/glib-2.0/schemas/org.gnome.shell.gschema.xml > /out/shell-schema-owner.txt
rpm -V gnome-shell-common > /out/shell-common-package-verify.txt
python3 - <<'SCHEMA' > /out/mutter-schema-preflight.txt
from gi.repository import Gio
source = Gio.SettingsSchemaSource.get_default()
for name in ['org.gnome.desktop.interface', 'org.gnome.mutter',
             'org.gnome.desktop.wm.preferences', 'org.gnome.shell.app-switcher',
             'org.gnome.desktop.session']:
    schema = source.lookup(name, True)
    if schema is None:
        raise RuntimeError('actual packaged schema is missing: '+name)
    print(schema.get_id())
    print('keys:', ','.join(sorted(schema.list_keys())))
SCHEMA
python3 /repo/scripts/lib/roost-svg-loader-receipt.py > /out/svg-loader-preflight.log 2>&1
pids=""
cleanup() { for pid in $pids; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT
Xvfb :99 -screen 0 1280x800x24 -ac -nolisten tcp >/out/xvfb.log 2>&1 & pids="$pids $!"
sleep 1
export WAYLAND_DISPLAY=svg-proof GDK_BACKEND=wayland
export ROOST_COMPOSITOR_STATE=/out/compositor-state.json
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY GDK_BACKEND GTK_A11Y GSETTINGS_BACKEND XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME LC_ALL
env -u WAYLAND_DISPLAY /candidate/usr/bin/roost-compositor --backend winit --socket svg-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/roost-shell-gtk >/out/compositor.log 2>&1 & compositor_pid=$!; pids="$pids $compositor_pid"
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/svg-proof" ] && [ -s "$ROOST_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
python3 /repo/scripts/lib/roost-stock-svg-proof.py initial > /out/journey-initial.log 2>&1
shell_pid=$(jq -r '.pid' /out/initial-session.json)
kill "$compositor_pid"
kill "$shell_pid" 2>/dev/null || true
wait "$compositor_pid" || true
# Once both actual owners are gone, remove their private stale socket/lock.
end=$((SECONDS + 15))
until gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.gnome.Shell | grep -q false; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
rm -f "$XDG_RUNTIME_DIR/svg-proof" "$XDG_RUNTIME_DIR/svg-proof.lock" "$ROOST_COMPOSITOR_STATE"
env -u WAYLAND_DISPLAY /candidate/usr/bin/roost-compositor --backend winit --socket svg-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/roost-shell-gtk >/out/compositor-restarted.log 2>&1 & pids="$pids $!"
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/svg-proof" ] && [ -s "$ROOST_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
python3 /repo/scripts/lib/roost-stock-svg-proof.py restarted > /out/journey-restarted.log 2>&1
printf '%s\n' 'GNOME51 stock SVG chooser: actual native SVG/font reference and desktop/lock pixels, real light/dark UI, same-URI rewrite/resource negatives and genuine persisted session restart' > /out/assertions.txt
