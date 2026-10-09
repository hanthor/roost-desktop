#!/usr/bin/python3
"""Real untrusted D-Bus caller, including forged portal well-known name."""
import socket
import struct
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
def ordinary_display_connections():
    # Two live displays from the same ordinary bus caller are allowed. The tag
    # is metadata only; the capture checks below still deny this same caller.
    sockets = []
    try:
        for tag in ("untrusted-test", "another-display"):
            reply, fds = bus.call_with_unix_fd_list_sync(
                "org.gnome.Mutter.ServiceChannel", "/org/gnome/Mutter/ServiceChannel",
                "org.gnome.Mutter.ServiceChannel", "OpenWaylandConnection",
                GLib.Variant("(a{sv})", ({"window-tag": GLib.Variant("s", tag)},)),
                GLib.VariantType.new("(h)"), Gio.DBusCallFlags.NONE, 10000, None, None)
            display = socket.socket(fileno=fds.get(reply.unpack()[0]))
            sockets.append(display)
            display.settimeout(10)
            display.sendall(struct.pack("=III", 1, 12 << 16, 2))
            events = b""
            while len(events) < 24:
                data = display.recv(24 - len(events))
                if not data:
                    raise RuntimeError("ordinary Wayland display closed before sync")
                events += data
            words = struct.unpack("=IIIIII", events)
            if words[:2] != (2, 12 << 16) or words[3:] != (1, (12 << 16) | 1, 2):
                raise RuntimeError("ordinary Wayland display did not complete actual sync")
        print("OpenWaylandConnection: two ordinary tagged displays complete real wl_display.sync")
    finally:
        for display in sockets:
            display.close()

ordinary_display_connections()
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
