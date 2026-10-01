#!/usr/bin/python3
"""System-service stubs for quick-settings proofs (#55).

Usage: roost-service-stubs.py BACKLIGHT_DIR

Serves, on the bus at $DBUS_SYSTEM_BUS_ADDRESS (a private bus the proof
starts), just enough of each daemon behind GNOME 51's quick settings:

- NetworkManager: WirelessEnabled (rw), GetDevices with one ethernet
  (activated) and one wifi device.
- BlueZ: ObjectManager plus /org/bluez/hci0 Adapter1.Powered (rw).
- power-profiles-daemon: ActiveProfile (rw), UPower name.
- logind: session/auto SetBrightness, which writes BACKLIGHT_DIR's
  brightness file the way the kernel would.

Writes emit PropertiesChanged, so the shell sees its own changes the
way it sees the real daemons'. The proof reads state back with gdbus.
"""
import os
import sys

from gi.repository import Gio, GLib

BACKLIGHT = sys.argv[1]

XML = """
<node>
  <interface name="org.freedesktop.NetworkManager">
    <method name="GetDevices"><arg type="ao" direction="out"/></method>
    <property name="WirelessEnabled" type="b" access="readwrite"/>
  </interface>
  <interface name="org.freedesktop.NetworkManager.Device">
    <property name="DeviceType" type="u" access="read"/>
    <property name="State" type="u" access="read"/>
  </interface>
  <interface name="org.freedesktop.DBus.ObjectManager">
    <method name="GetManagedObjects">
      <arg type="a{oa{sa{sv}}}" direction="out"/>
    </method>
  </interface>
  <interface name="org.bluez.Adapter1">
    <property name="Powered" type="b" access="readwrite"/>
  </interface>
  <interface name="org.freedesktop.UPower.PowerProfiles">
    <property name="ActiveProfile" type="s" access="readwrite"/>
  </interface>
  <interface name="org.freedesktop.login1.Session">
    <method name="SetBrightness">
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="u" direction="in"/>
    </method>
  </interface>
</node>
"""
NODE = Gio.DBusNodeInfo.new_for_xml(XML)

NM = "/org/freedesktop/NetworkManager"
DEV_ETH = NM + "/Devices/1"
DEV_WIFI = NM + "/Devices/2"
HCI = "/org/bluez/hci0"
PPD = "/org/freedesktop/UPower/PowerProfiles"
SESSION = "/org/freedesktop/login1/session/auto"

# (path, interface) -> {property: GLib.Variant}
state = {
    (NM, "org.freedesktop.NetworkManager"): {"WirelessEnabled": GLib.Variant("b", True)},
    (DEV_ETH, "org.freedesktop.NetworkManager.Device"): {
        "DeviceType": GLib.Variant("u", 1), "State": GLib.Variant("u", 100)},
    (DEV_WIFI, "org.freedesktop.NetworkManager.Device"): {
        "DeviceType": GLib.Variant("u", 2), "State": GLib.Variant("u", 30)},
    (HCI, "org.bluez.Adapter1"): {"Powered": GLib.Variant("b", True)},
    (PPD, "org.freedesktop.UPower.PowerProfiles"): {"ActiveProfile": GLib.Variant("s", "balanced")},
}

conn = Gio.DBusConnection.new_for_address_sync(
    os.environ["DBUS_SYSTEM_BUS_ADDRESS"],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
    | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)


def method_call(c, sender, path, iface, method, params, invocation):
    if method == "GetDevices":
        invocation.return_value(GLib.Variant("(ao)", ([DEV_ETH, DEV_WIFI],)))
    elif method == "GetManagedObjects":
        props = state[(HCI, "org.bluez.Adapter1")]
        invocation.return_value(GLib.Variant(
            "(a{oa{sa{sv}}})", ({HCI: {"org.bluez.Adapter1": props}},)))
    elif method == "SetBrightness":
        _, _, value = params.unpack()
        with open(os.path.join(BACKLIGHT, "brightness"), "w") as fh:
            fh.write(f"{value}\n")
        invocation.return_value(None)
    else:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)


def get_property(c, sender, path, iface, prop):
    return state[(path, iface)][prop]


def set_property(c, sender, path, iface, prop, value):
    state[(path, iface)][prop] = value
    c.emit_signal(None, path, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                  GLib.Variant("(sa{sv}as)", (iface, {prop: value}, [])))
    return True


def serve(path, iface):
    info = NODE.lookup_interface(iface)
    conn.register_object(path, info, method_call, get_property, set_property)


for path, iface in [
    (NM, "org.freedesktop.NetworkManager"),
    (DEV_ETH, "org.freedesktop.NetworkManager.Device"),
    (DEV_WIFI, "org.freedesktop.NetworkManager.Device"),
    ("/", "org.freedesktop.DBus.ObjectManager"),
    (HCI, "org.bluez.Adapter1"),
    (PPD, "org.freedesktop.UPower.PowerProfiles"),
    (SESSION, "org.freedesktop.login1.Session"),
]:
    serve(path, iface)

for name in ["org.freedesktop.NetworkManager", "org.bluez",
             "org.freedesktop.UPower.PowerProfiles", "org.freedesktop.login1"]:
    conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                   "RequestName", GLib.Variant("(su)", (name, 4)), None,
                   Gio.DBusCallFlags.NONE, -1, None)

print("roost-service-stubs: ready", flush=True)
GLib.MainLoop().run()
