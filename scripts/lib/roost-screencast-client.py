#!/usr/bin/python3
"""Screen-cast client for proofs (#61): what xdg-desktop-portal-gnome does.

Usage: roost-screencast-client.py OUT.png [--window]

Lists monitors over org.gnome.Mutter.DisplayConfig, records the first one
over org.gnome.Mutter.ScreenCast, waits for PipeWireStreamAdded, grabs
one frame from the PipeWire node with GStreamer, and stops the session.
Prints "node <id> <connector>" on success; exits 1 otherwise.

With --window it shares a window instead, the portal's way: checks that
org.gnome.Shell.Introspect refuses a caller that is not the desktop
portal, takes the portal's bus name, lists windows, records the first
test window with RecordWindow, and prints
"node <id> window <window-id> <width>x<height> <title>".
"""
import subprocess
import sys

from gi.repository import Gio, GLib

OUT = sys.argv[1]
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)


def call(dest, path, iface, method, args, reply):
    return bus.call_sync(dest, path, iface, method, args, GLib.VariantType(reply),
                         Gio.DBusCallFlags.NONE, 10000, None).unpack()


serial, monitors, logical, props = call(
    "org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig",
    "org.gnome.Mutter.DisplayConfig", "GetCurrentState", None,
    "(ua((ssss)a(siiddada{sv})a{sv})a(iiduba(ssss)a{sv})a{sv})")
if not monitors:
    print("no monitors", file=sys.stderr)
    sys.exit(1)
connector = monitors[0][0][0]
WINDOW = "--window" in sys.argv[2:]
INTROSPECT = ("org.gnome.Shell.Introspect", "/org/gnome/Shell/Introspect",
              "org.gnome.Shell.Introspect", "GetWindows", None, "(a{ta{sv}})")
chosen = None
if WINDOW:
    try:
        call(*INTROSPECT)
        print("Introspect answered a caller that is not the portal", file=sys.stderr)
        sys.exit(1)
    except GLib.Error as e:
        if "AccessDenied" not in e.message:
            print(f"unexpected Introspect error: {e.message}", file=sys.stderr)
            sys.exit(1)
    # Become the desktop portal, as xdg-desktop-portal-gnome is.
    (reply,) = call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                    "RequestName",
                    GLib.Variant("(su)", ("org.freedesktop.impl.portal.desktop.gnome", 4)),
                    "(u)")
    if reply != 1:
        print(f"could not own the portal name ({reply})", file=sys.stderr)
        sys.exit(1)
    (windows,) = call(*INTROSPECT)
    tests = [(wid, p) for wid, p in sorted(windows.items())
             if p.get("app-id", "").startswith("roost-test-")]
    if not tests:
        print(f"no test window in GetWindows: {windows}", file=sys.stderr)
        sys.exit(1)
    chosen = tests[0]
(session,) = call("org.gnome.Mutter.ScreenCast", "/org/gnome/Mutter/ScreenCast",
                  "org.gnome.Mutter.ScreenCast", "CreateSession",
                  GLib.Variant("(a{sv})", ({},)), "(o)")
if chosen:
    (stream,) = call("org.gnome.Mutter.ScreenCast", session,
                     "org.gnome.Mutter.ScreenCast.Session", "RecordWindow",
                     GLib.Variant("(a{sv})", ({"window-id": GLib.Variant("t", chosen[0])},)),
                     "(o)")
else:
    (stream,) = call("org.gnome.Mutter.ScreenCast", session,
                     "org.gnome.Mutter.ScreenCast.Session", "RecordMonitor",
                     GLib.Variant("(sa{sv})", (connector, {})), "(o)")
node = []
loop = GLib.MainLoop()


def on_added(_conn, _sender, _path, _iface, _signal, params):
    node.append(params.unpack()[0])
    loop.quit()


bus.signal_subscribe(None, "org.gnome.Mutter.ScreenCast.Stream", "PipeWireStreamAdded",
                     stream, None, Gio.DBusSignalFlags.NONE, on_added)
call("org.gnome.Mutter.ScreenCast", session, "org.gnome.Mutter.ScreenCast.Session",
     "Start", None, "()")
GLib.timeout_add_seconds(15, loop.quit)
loop.run()
if not node:
    print("no PipeWireStreamAdded", file=sys.stderr)
    sys.exit(1)
grab = subprocess.run(
    ["gst-launch-1.0", "-q", "pipewiresrc", f"path={node[0]}", "num-buffers=1", "!",
     "videoconvert", "!", "pngenc", "!", "filesink", f"location={OUT}"],
    timeout=30, capture_output=True, text=True)
call("org.gnome.Mutter.ScreenCast", session, "org.gnome.Mutter.ScreenCast.Session",
     "Stop", None, "()")
if grab.returncode != 0:
    print(f"gst failed: {grab.stderr}", file=sys.stderr)
    sys.exit(1)
if chosen:
    wid, props = chosen
    print(f"node {node[0]} window {wid} {props['width']}x{props['height']} {props['title']}")
else:
    print(f"node {node[0]} {connector}")
