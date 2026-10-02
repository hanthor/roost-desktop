#!/usr/bin/python3
"""A stand-in for gnome-shell's calendar server, for harnesses.

Usage: roost-calendar-server-stub.py [EVENTS_JSON]

Owns org.gnome.Shell.CalendarServer on the session bus, as gnome-shell's
calendar server does, with HasCalendars true (a session with its default
local calendar). Each SetTimeRange answers with EventsAddedOrUpdated for
the events inside the range. EVENTS_JSON is a list of
{"summary": str, "day": int, "start": "HH:MM", "end": "HH:MM"}, where
"day" counts from today and events without "start" last the whole day.
Without it there are no events, as on a fresh GNOME session.
"""
import datetime
import json
import sys

from gi.repository import Gio, GLib

XML = """
<node><interface name="org.gnome.Shell.CalendarServer">
  <method name="SetTimeRange">
    <arg type="x" name="since" direction="in"/>
    <arg type="x" name="until" direction="in"/>
    <arg type="b" name="force_reload" direction="in"/>
  </method>
  <signal name="EventsAddedOrUpdated"><arg type="a(ssxxa{sv})" name="events"/></signal>
  <signal name="EventsRemoved"><arg type="as" name="ids"/></signal>
  <signal name="ClientDisappeared"><arg type="s" name="source_uid"/></signal>
  <property name="HasCalendars" type="b" access="read"/>
</interface></node>
"""
PATH = "/org/gnome/Shell/CalendarServer"
IFACE = "org.gnome.Shell.CalendarServer"


def load_events(path):
    if not path:
        return []
    today = datetime.date.today()
    out = []
    with open(path, encoding="utf-8") as fh:
        for n, item in enumerate(json.load(fh)):
            day = today + datetime.timedelta(days=item.get("day", 0))
            if "start" in item:
                start = datetime.datetime.combine(
                    day, datetime.time.fromisoformat(item["start"]))
                end = datetime.datetime.combine(
                    day, datetime.time.fromisoformat(item.get("end", item["start"])))
            else:
                start = datetime.datetime.combine(day, datetime.time())
                end = start + datetime.timedelta(days=1)
            # Local times, as the server reports them in Unix seconds.
            out.append((f"stub\n{n}\n", item["summary"],
                        int(start.timestamp()), int(end.timestamp())))
    return out


EVENTS = load_events(sys.argv[1] if len(sys.argv) > 1 else None)
NODE = Gio.DBusNodeInfo.new_for_xml(XML)


def method_call(conn, sender, path, iface, method, params, invocation):
    if method == "SetTimeRange":
        since, until, _force = params.unpack()
        inside = [(i, s, a, b, {}) for (i, s, a, b) in EVENTS if a < until and b > since]
        invocation.return_value(None)
        if inside:
            conn.emit_signal(None, PATH, IFACE, "EventsAddedOrUpdated",
                             GLib.Variant("(a(ssxxa{sv}))", (inside,)))
    else:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)


def get_property(conn, sender, path, iface, prop):
    return GLib.Variant("b", True)


def on_bus(conn, _name):
    conn.register_object(PATH, NODE.interfaces[0], method_call, get_property, None)


def on_name(_conn, _name):
    print("roost-calendar-server-stub: ready", flush=True)


Gio.bus_own_name(Gio.BusType.SESSION, "org.gnome.Shell.CalendarServer",
                 Gio.BusNameOwnerFlags.NONE, on_bus, on_name, None)
GLib.MainLoop().run()
