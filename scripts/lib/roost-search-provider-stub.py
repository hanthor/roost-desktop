#!/usr/bin/python3
"""A GNOME Shell search provider for overview proofs (#57).

Usage: roost-search-provider-stub.py MARKER

Serves org.gnome.Shell.SearchProvider2 as org.roost.ProbeSearch on the
session bus. Terms that include "report" find one document; activating
it (or launching the search) writes "<method> <id> <terms>" to MARKER.
"""
import sys

from gi.repository import Gio, GLib

MARKER = sys.argv[1]
XML = """
<node><interface name="org.gnome.Shell.SearchProvider2">
  <method name="GetInitialResultSet">
    <arg type="as" direction="in"/><arg type="as" direction="out"/></method>
  <method name="GetSubsearchResultSet">
    <arg type="as" direction="in"/><arg type="as" direction="in"/>
    <arg type="as" direction="out"/></method>
  <method name="GetResultMetas">
    <arg type="as" direction="in"/><arg type="aa{sv}" direction="out"/></method>
  <method name="ActivateResult">
    <arg type="s" direction="in"/><arg type="as" direction="in"/>
    <arg type="u" direction="in"/></method>
  <method name="LaunchSearch">
    <arg type="as" direction="in"/><arg type="u" direction="in"/></method>
</interface></node>
"""
DOCS = {"doc-1": ("Quarterly Report.odt", "~/Documents/Work")}


def hits(terms):
    return ["doc-1"] if any("report" in t for t in terms) else []


def method_call(conn, sender, path, iface, method, params, invocation):
    args = params.unpack()
    if method == "GetInitialResultSet":
        invocation.return_value(GLib.Variant("(as)", (hits(args[0]),)))
    elif method == "GetSubsearchResultSet":
        invocation.return_value(GLib.Variant("(as)", (hits(args[1]),)))
    elif method == "GetResultMetas":
        metas = [{"id": GLib.Variant("s", i),
                  "name": GLib.Variant("s", DOCS[i][0]),
                  "description": GLib.Variant("s", DOCS[i][1])}
                 for i in args[0] if i in DOCS]
        invocation.return_value(GLib.Variant("(aa{sv})", (metas,)))
    elif method == "ActivateResult":
        with open(MARKER, "w") as fh:
            fh.write(f"activate {args[0]} {' '.join(args[1])}\n")
        invocation.return_value(None)
    elif method == "LaunchSearch":
        with open(MARKER, "w") as fh:
            fh.write(f"launch - {' '.join(args[0])}\n")
        invocation.return_value(None)


def on_bus(conn, name):
    info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
    conn.register_object("/org/roost/ProbeSearch", info, method_call, None, None)


def on_name(conn, name):
    print("roost-search-provider-stub: ready", flush=True)


Gio.bus_own_name(Gio.BusType.SESSION, "org.roost.ProbeSearch",
                 Gio.BusNameOwnerFlags.NONE, on_bus, on_name, None)
GLib.MainLoop().run()
