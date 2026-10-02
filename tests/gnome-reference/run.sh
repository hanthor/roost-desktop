#!/bin/sh
# Inside the reference container: a headless GNOME 51 session running
# capture.js, with Roost's proof stubs (NetworkManager, BlueZ, power
# profiles, logind) on a private system bus, as the Roost capture has.
set -eu
export XDG_RUNTIME_DIR=/tmp/xdg GREF_OUT=/out LANG=C.UTF-8
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
# The "needs GDM" notice would sit in every frame.
mkdir -p /root/.local/share/gnome-shell
touch /root/.local/share/gnome-shell/lock-warning-shown
# GNOME 51's default wallpapers, for the Roost capture to show too.
cp /usr/share/backgrounds/gnome/adwaita-l.jxl /usr/share/backgrounds/gnome/adwaita-d.jxl /out/
mkdir -p /tmp/backlight
echo 500 >/tmp/backlight/brightness
echo 1000 >/tmp/backlight/max_brightness
dbus-daemon --session --address=unix:path=/tmp/sysbus --fork --nopidfile
export DBUS_SYSTEM_BUS_ADDRESS=unix:path=/tmp/sysbus
python3 /lib/roost-service-stubs.py /tmp/backlight >/tmp/stubs.log 2>&1 &
sleep 1
# shellcheck disable=SC2016 # expands in the inner shell
exec dbus-run-session -- sh -c '
  gsettings set org.gnome.desktop.interface enable-animations false
  gsettings set org.gnome.shell welcome-dialog-last-shown-version "999"
  timeout 240 gnome-shell --headless --wayland --virtual-monitor "${GREF_SIZE:-1280x800}" \
      --automation-script /gref/capture.js
'
