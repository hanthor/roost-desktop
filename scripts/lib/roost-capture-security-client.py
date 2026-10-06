#!/usr/bin/python3
"""Real untrusted D-Bus caller, including forged portal well-known name."""
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
def denied(dest, path, iface, method, args):
    try:
        bus.call_sync(dest, path, iface, method, args, None, Gio.DBusCallFlags.NONE, 10000, None)
    except GLib.Error as error:
        if "AccessDenied" not in error.message: raise
        print(method + ": denied")
        return
    raise RuntimeError(method + " accepted an untrusted caller")
def checks():
    for role in (1, 2, 3):
        denied("org.gnome.Mutter.ServiceChannel", "/org/gnome/Mutter/ServiceChannel", "org.gnome.Mutter.ServiceChannel", "OpenWaylandServiceConnection", GLib.Variant("(u)", (role,)))
    denied("org.gnome.Shell.Introspect", "/org/gnome/Shell/Introspect", "org.gnome.Shell.Introspect", "GetWindows", None)
    denied("org.gnome.Mutter.ScreenCast", "/org/gnome/Mutter/ScreenCast", "org.gnome.Mutter.ScreenCast", "CreateSession", GLib.Variant("(a{sv})", ({},)))
    denied("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot", "org.gnome.Shell.Screenshot", "Screenshot", GLib.Variant("(bbs)", (False, False, "/tmp/untrusted-capture.png")))
    denied("org.gnome.Shell.Screencast", "/org/gnome/Shell/Screencast", "org.gnome.Shell.Screencast", "Screencast", GLib.Variant("(sa{sv})", ("untrusted", {})))
checks()
reply = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", ("org.freedesktop.impl.portal.desktop.gnome", 4)), None, Gio.DBusCallFlags.NONE, 10000, None).unpack()[0]
if reply != 1:
    print("real portal already owns name; forged claim refused")
else:
    checks()
    print("forged portal name cannot grant capture")

for name in ("org.gnome.Settings.GlobalShortcutsProvider", "org.gnome.Nautilus"):
    reply = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", (name, 4)), None, Gio.DBusCallFlags.NONE, 10000, None).unpack()[0]
    if reply == 1:
        checks()
        print("forged " + name + " cannot grant typed bootstrap or capture")
        bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName", GLib.Variant("(s)", (name,)), None, Gio.DBusCallFlags.NONE, 10000, None)
