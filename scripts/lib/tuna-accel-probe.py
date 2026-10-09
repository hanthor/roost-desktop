#!/usr/bin/python3
"""Stand in for gnome-settings-daemon's media keys in proof harnesses.

Usage: tuna-accel-probe.py MARKER

Owns org.gnome.SettingsDaemon.MediaKeys (so org.gnome.Shell's caller
allowlist admits it), grabs XF86AudioPlay through
org.gnome.Shell.GrabAccelerator, and on AcceleratorActivated shows the
volume OSD through ShowOSD, as gsd-media-keys does. Progress lines go to
MARKER: "grabbed N", then "activated N".
"""
import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

MARKER = sys.argv[1]
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
loop = GLib.MainLoop()
action = 0


def note(line):
    with open(MARKER, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")


def call(method, args, reply):
    return bus.call_sync("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell",
                         method, args, GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 2000, None)


def on_signal(_conn, _sender, _path, _iface, _name, params):
    fired, info = params.unpack()
    if fired != action:
        return
    note(f"activated {fired} mode {info.get('action-mode')}")
    call("ShowOSD", GLib.Variant("(a{sv})", ({
        "icon": GLib.Variant("s", "audio-volume-high-symbolic"),
        "level": GLib.Variant("d", 0.75),
    },)), None)


def grab():
    global action
    try:
        action = call("GrabAccelerator",
                      GLib.Variant("(suu)", ("XF86AudioPlay", 1, 0)), "(u)").unpack()[0]
    except GLib.Error as e:
        note(f"error {e.message}")
        return True
    note(f"grabbed {action}")
    return False


def owned(*_):
    bus.signal_subscribe("org.gnome.Shell", "org.gnome.Shell", "AcceleratorActivated",
                         "/org/gnome/Shell", None, Gio.DBusSignalFlags.NONE, on_signal)
    if grab():
        GLib.timeout_add(500, grab)


Gio.bus_own_name_on_connection(bus, "org.gnome.SettingsDaemon.MediaKeys",
                               Gio.BusNameOwnerFlags.NONE, owned, None)
GLib.timeout_add_seconds(120, loop.quit)
loop.run()
