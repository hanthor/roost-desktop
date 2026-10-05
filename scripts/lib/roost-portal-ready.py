#!/usr/bin/python3
"""Require real ScreenCast capabilities before starting a portal journey."""
import json
from pathlib import Path
import sys
import time
from gi.repository import Gio, GLib

service, interface, output = sys.argv[1:]
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
deadline = time.monotonic() + 30
last = "no capabilities returned"
while time.monotonic() < deadline:
    try:
        reply = bus.call_sync(
            service, "/org/freedesktop/portal/desktop",
            "org.freedesktop.DBus.Properties", "GetAll",
            GLib.Variant("(s)", (interface,)), None,
            Gio.DBusCallFlags.NO_AUTO_START, 500, None,
        )
        properties = reply.unpack()[0]
        last = json.dumps(properties, sort_keys=True)
        if (int(properties.get("version", 0)) >= 2
                and int(properties.get("AvailableCursorModes", 0)) & 1
                and int(properties.get("AvailableSourceTypes", 0)) & 1):
            Path(output).write_text(json.dumps(properties, indent=2) + "\n")
            print(service + ": real monitor and hidden-cursor capabilities ready")
            sys.exit(0)
    except GLib.Error as error:
        last = error.message
    time.sleep(.1)
raise RuntimeError(service + " ScreenCast capabilities did not become ready: " + last)
