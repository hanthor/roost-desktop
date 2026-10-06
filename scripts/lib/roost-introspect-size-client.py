#!/usr/bin/python3
"""Observe real ScreenSize notifications while the graphical proof changes scale."""
import json
import sys
from pathlib import Path

from gi.repository import Gio, GLib

report = Path(sys.argv[1])
width, height = map(int, sys.argv[2:])
name = "org.gnome.Shell.Introspect"
path = "/org/gnome/Shell/Introspect"
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
expected = [(round(width / 1.25), round(height / 1.25)), (width, height)]
observed = []
error = None
loop = GLib.MainLoop()


def read_size():
    return tuple(bus.call_sync(
        name, path, "org.freedesktop.DBus.Properties", "Get",
        GLib.Variant("(ss)", (name, "ScreenSize")),
        GLib.VariantType.new("(v)"), Gio.DBusCallFlags.NONE, 5000, None
    ).unpack()[0])


def changed(_bus, _sender, _path, _interface, _signal, parameters, _data):
    global error
    try:
        interface, values, invalidated = parameters.unpack()
        if interface != name or "ScreenSize" not in values:
            return
        actual = tuple(values["ScreenSize"])
        index = len(observed)
        if (index >= len(expected) or actual != expected[index]
                or "ScreenSize" in invalidated or read_size() != actual):
            raise RuntimeError(f"unexpected size notification/property: {actual}")
        observed.append(actual)
        report.write_text(json.dumps({"initial": [width, height], "changes": observed}) + "\n")
        if index == 0:
            Path(str(report) + ".scaled").touch()
        else:
            loop.quit()
    except Exception as exc:
        error = exc
        loop.quit()


def timed_out():
    global error
    error = RuntimeError(f"missing ScreenSize notifications: {observed}")
    loop.quit()
    return GLib.SOURCE_REMOVE


subscription = bus.signal_subscribe(
    name, "org.freedesktop.DBus.Properties", "PropertiesChanged", path,
    name, Gio.DBusSignalFlags.NONE, changed, None)
try:
    # Round trip orders the subscription before the parent can change scale.
    if read_size() != (width, height):
        raise RuntimeError("initial desktop size disagrees with the real display")
    Path(str(report) + ".ready").touch()
    GLib.timeout_add_seconds(25, timed_out)
    loop.run()
    if error:
        raise error
    if observed != expected:
        raise RuntimeError(f"incomplete size journey: {observed}")
    print("ScreenSize: initial property and two live scale notifications agree")
finally:
    bus.signal_unsubscribe(subscription)
