#!/usr/bin/python3
"""Require real ScreenCast capabilities before starting a portal journey."""
import argparse
import json
from pathlib import Path
import sys
import time
from gi.repository import Gio, GLib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("service")
parser.add_argument("interface")
parser.add_argument("output")
parser.add_argument("--pid", type=int, help="Require this process to own the service")
args = parser.parse_args()
service, interface, output = args.service, args.interface, args.output
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
deadline = time.monotonic() + 30
last = "no capabilities returned"
while time.monotonic() < deadline:
    try:
        def bus_property(method, signature, value):
            return bus.call_sync(
                "org.freedesktop.DBus", "/org/freedesktop/DBus",
                "org.freedesktop.DBus", method,
                GLib.Variant(signature, value), None,
                Gio.DBusCallFlags.NO_AUTO_START, 500, None,
            ).unpack()[0]

        owner = bus_property("GetNameOwner", "(s)", (service,))
        pid = bus_property("GetConnectionUnixProcessID", "(s)", (owner,))
        if args.pid is not None and pid != args.pid:
            last = f"owner PID {pid}, waiting for PID {args.pid}"
            time.sleep(.1)
            continue
        # Query the checked unique owner, never a replacement that took the
        # well-known name between the process check and the properties call.
        reply = bus.call_sync(
            owner, "/org/freedesktop/portal/desktop",
            "org.freedesktop.DBus.Properties", "GetAll",
            GLib.Variant("(s)", (interface,)), None,
            Gio.DBusCallFlags.NO_AUTO_START, 500, None,
        )
        properties = reply.unpack()[0]
        last = json.dumps(properties, sort_keys=True)
        if owner != bus_property("GetNameOwner", "(s)", (service,)):
            last = "owner changed during capability probe"
            continue
        if (int(properties.get("version", 0)) >= 2
                and int(properties.get("AvailableCursorModes", 0)) & 1
                and int(properties.get("AvailableSourceTypes", 0)) & 1):
            properties["owner"] = owner
            properties["owner_pid"] = pid
            Path(output).write_text(json.dumps(properties, indent=2) + "\n")
            print(service + ": real monitor and hidden-cursor capabilities ready")
            sys.exit(0)
    except GLib.Error as error:
        last = error.message
    time.sleep(.1)
raise RuntimeError(service + " ScreenCast capabilities did not become ready: " + last)
