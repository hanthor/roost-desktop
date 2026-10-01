#!/bin/sh
# Inside the reference container: a headless GNOME 51 session running
# capture.js, with Roost's proof stubs (power profiles, logind) on a
# private system bus, as the Roost capture has.
set -eu
export XDG_RUNTIME_DIR=/tmp/xdg GREF_OUT=/out LANG=C.UTF-8
# As in a GNOME session: GNOME's schema overrides (dynamic workspaces...)
# apply only under this desktop name.
export XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_DESKTOP=gnome
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
# The "needs GDM" notice would sit in every frame.
mkdir -p /root/.local/share/gnome-shell
touch /root/.local/share/gnome-shell/lock-warning-shown
# GNOME 51's icons and fonts, for the Roost capture to render with
# (a host's older Adwaita lacks icons such as dark-mode-symbolic).
mkdir -p /out/share/icons /out/share/fonts
cp -r /usr/share/icons/Adwaita /usr/share/icons/hicolor /out/share/icons/
cp -r /usr/share/fonts/adwaita-sans-fonts /usr/share/fonts/adwaita-mono-fonts /out/share/fonts/
# GNOME Shell's settings schema, for hosts without gnome-shell.
mkdir -p /out/share/glib-2.0/schemas
cp /usr/share/glib-2.0/schemas/org.gnome.shell.gschema.xml /out/share/glib-2.0/schemas/
# The dash's favorites that GNOME 51's default list finds installed.
mkdir -p /out/share/applications
cp /usr/share/applications/org.gnome.Nautilus.desktop /usr/share/applications/org.gnome.TextEditor.desktop \
    /usr/share/applications/org.gnome.Calculator.desktop /out/share/applications/
# GNOME 51's default wallpapers, for the Roost capture to show too.
cp /usr/share/backgrounds/gnome/adwaita-l.jxl /usr/share/backgrounds/gnome/adwaita-d.jxl /out/
mkdir -p /tmp/backlight
echo 500 >/tmp/backlight/brightness
echo 1000 >/tmp/backlight/max_brightness
dbus-daemon --session --address=unix:path=/tmp/sysbus --fork --nopidfile
export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/tmp/sysbus
# GNOME Shell locks only under a display manager on systemd: the gdm
# stub answers its probe, and systemd's seat directory must exist.
mkdir -p /run/systemd/seats
# Only what GNOME's headless session can use, so Roost's capture can run
# the same: power profiles and logind (its NetworkManager and BlueZ
# clients need more than the stubs offer).
ROOST_STUB_SERVICES=ppd,logind,gdm python3 /lib/roost-service-stubs.py /tmp/backlight >/tmp/stubs.log 2>&1 &
sleep 1
# shellcheck disable=SC2016 # expands in the inner shell
exec dbus-run-session -- sh -c '
  gsettings set org.gnome.desktop.interface enable-animations false
  gsettings set org.gnome.shell welcome-dialog-last-shown-version "999"
  timeout 240 gnome-shell --headless --wayland --virtual-monitor "${GREF_SIZE:-1280x800}" \
      --automation-script /gref/capture.js
'
