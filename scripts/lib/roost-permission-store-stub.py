#!/usr/bin/python3
"""Private-session PermissionStore for the live shortcut consent proof."""
import json
import sys
from gi.repository import Gio, GLib

BUS = "org.freedesktop.impl.portal.PermissionStore"
PATH = "/org/freedesktop/impl/portal/PermissionStore"
permissions = {}
node = Gio.DBusNodeInfo.new_for_xml('''<node><interface name="org.freedesktop.impl.portal.PermissionStore">
<method name="Lookup"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sas}" direction="out"/><arg type="v" direction="out"/></method>
<method name="SetPermission"><arg type="s" direction="in"/><arg type="b" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/></method>
</interface></node>''')

def call(_conn, _sender, _path, _interface, method, params, invocation):
    args = params.unpack()
    if args[0] != "gnome" or args[2 if method == "SetPermission" else 1] != "shortcuts-inhibitor":
        invocation.return_dbus_error("org.freedesktop.portal.Error.NotFound", "Unknown table/id")
        return
    if method == "Lookup":
        invocation.return_value(GLib.Variant("(a{sas}v)", (permissions, GLib.Variant("s", ""))))
    else:
        permissions[args[3]] = args[4]
        with open(sys.argv[1], "w", encoding="utf-8") as out:
            json.dump(permissions, out)
        invocation.return_value(GLib.Variant("()", ()))

connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
connection.register_object(PATH, node.interfaces[0], call, None, None)
reply = connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", (BUS, 4)), None, Gio.DBusCallFlags.NONE, -1, None)
if reply.unpack()[0] not in (1, 4):
    raise RuntimeError("PermissionStore already has an owner on the proof bus")
print("ready", flush=True)
GLib.MainLoop().run()
