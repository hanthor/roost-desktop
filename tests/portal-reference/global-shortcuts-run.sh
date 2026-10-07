#!/usr/bin/env bash
set -euo pipefail
if [ "$(id -u)" -eq 0 ]; then
    # A real isolated system bus, without fabricated service implementations.
    mkdir -p /run/dbus
    dbus-daemon --system --fork --nopidfile
    chown -R --no-dereference roost-proof:roost-proof /out
    status=0
    env -u DBUS_SESSION_BUS_ADDRESS -u ROOST_GLOBAL_SHORTCUTS_BUS runuser -u roost-proof -- "$0" || status=$?
    finalization=0
    python3 /repo/scripts/lib/roost-portal-artifact-finalize.py /out --proof-status "$status" || finalization=$?
    [ "$status" -eq 0 ] || exit "$status"
    exit "$finalization"
fi
if [ -z "${ROOST_GLOBAL_SHORTCUTS_BUS:-}" ]; then ROOST_GLOBAL_SHORTCUTS_BUS=1 exec dbus-run-session -- "$0"; fi
[ "$(id -u)" -eq 1000 ]
export DISPLAY=:99 XDG_RUNTIME_DIR=/out/runtime XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME GSETTINGS_BACKEND=keyfile LIBGL_ALWAYS_SOFTWARE=1 GTK_A11Y=atspi LC_ALL=C.UTF-8
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
id > /out/session-identity.txt
rpm -q xdg-desktop-portal-gnome xdg-desktop-portal gnome-settings-daemon > /out/shortcut-runtime-versions.txt
[ "$(rpm -q --qf '%{VERSION}' xdg-desktop-portal-gnome | cut -d. -f1)" = 51 ]
[ "$(rpm -q --qf '%{VERSION}' gnome-settings-daemon | cut -d. -f1)" = 51 ]
rpm -V gnome-settings-daemon > /out/shortcut-schema-package-verify.txt
rpm -q gnome-control-center mutter-common gnome-shell-common gsettings-desktop-schemas gtk4 libadwaita at-spi2-core > /out/runtime-versions.txt
[ "$(rpm -q --qf '%{VERSION}' gnome-control-center | cut -d. -f1)" = 51 ]
rpm -V gnome-control-center > /out/settings-package-verify.txt
[ "$(rpm -q --qf '%{VERSION}' mutter-common | cut -d. -f1)" = 51 ]
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
             'org.gnome.desktop.session', 'org.gnome.settings-daemon.global-shortcuts',
             'org.gnome.settings-daemon.global-shortcuts.application']:
    schema = source.lookup(name, True)
    if schema is None:
        raise RuntimeError('actual packaged schema is missing: '+name)
    print(schema.get_id())
    print('keys:', ','.join(sorted(schema.list_keys())))
SCHEMA
pids=""
cleanup() { for pid in $pids; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT
Xvfb :99 -screen 0 1280x800x24 -ac -nolisten tcp >/out/xvfb.log 2>&1 & pids="$pids $!"
printf '%s\n' "$!" > /out/xvfb.pid
tr '\0' ' ' <"/proc/$!/cmdline" > /out/xvfb-command.txt
sleep 1
pipewire >/out/pipewire.log 2>&1 & pids="$pids $!"
wireplumber >/out/wireplumber.log 2>&1 & pids="$pids $!"
export WAYLAND_DISPLAY=global-shortcuts-proof GDK_BACKEND=wayland
export ROOST_COMPOSITOR_STATE=/out/compositor-state.json
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY GDK_BACKEND GTK_A11Y GSETTINGS_BACKEND XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME LC_ALL
env -u WAYLAND_DISPLAY /candidate/usr/bin/roost-compositor --backend winit --socket global-shortcuts-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/roost-shell-gtk >/out/compositor.log 2>&1 & pids="$pids $!"
printf '%s\n' "$!" > /out/compositor.pid
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/global-shortcuts-proof" ] && [ -s "$ROOST_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend.log 2>&1 & pids="$pids $!"
python3 /repo/scripts/lib/roost-gnome-global-shortcuts-proof.py > /out/journey.log 2>&1
printf '%s\n' 'GNOME51 genuine backend/provider: actual Add/Cancel decisions, shared persisted binding, physical held Activated then released Deactivated, denied and closed-session negative controls; backend-only explicit app ID, frontend native/Flatpak identity and rebind/restart/shipping remain unqualified' > /out/assertions.txt
