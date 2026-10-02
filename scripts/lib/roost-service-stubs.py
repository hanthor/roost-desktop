#!/usr/bin/python3
"""System-service stubs for quick-settings proofs (#55).

Usage: roost-service-stubs.py BACKLIGHT_DIR [POWER_LOG]

Serves, on the bus at $DBUS_SYSTEM_BUS_ADDRESS (a private bus the proof
starts), just enough of each daemon behind GNOME 51's quick settings:

- NetworkManager: WirelessEnabled (rw), GetDevices with one ethernet
  (activated) and one wifi device. The wifi device sees four access
  points (a saved WPA2 "Roost Home", an open "Roost Cafe", an 802.1X
  "Roost Office" and a hidden one); RequestScan, ActivateConnection and
  AddAndActivateConnection are appended to $ROOST_STUB_NM_LOG, and an
  activation makes its access point the active one.
- BlueZ: ObjectManager plus /org/bluez/hci0 Adapter1.Powered (rw), and
  three devices (paired Headphones, paired and connected Keyboard, an
  unpaired Stranger) whose Device1.Connect and Disconnect flip Connected
  and are appended to $ROOST_STUB_BT_LOG.
- power-profiles-daemon: ActiveProfile (rw), Profiles (power-saver,
  balanced, performance) and PerformanceDegraded, UPower name.
- logind: session/auto SetBrightness, which writes BACKLIGHT_DIR's
  brightness file the way the kernel would, and Terminate; the Manager's
  Suspend, Reboot and PowerOff. Power calls are appended to POWER_LOG,
  and Suspend emits PrepareForSleep(true) first, as logind does.

- polkit: the Authority's RegisterAuthenticationAgent, recording the
  agent's bus name and object path in $ROOST_STUB_POLKIT_LOG (one line,
  "NAME PATH") so a proof can call the agent's BeginAuthentication.

ROOST_STUB_SERVICES (comma-separated: nm, bluez, ppd, logind, polkit, gdm;
default all but gdm; gdm answers GNOME Shell's "can lock" probe, the
display manager's Version, so a reference GNOME session can lock) limits which daemons are served, so a capture can match another
session's services exactly.

Writes emit PropertiesChanged, so the shell sees its own changes the
way it sees the real daemons'. The proof reads state back with gdbus.
"""
import os
import sys

from gi.repository import Gio, GLib

BACKLIGHT = sys.argv[1]
POWER_LOG = sys.argv[2] if len(sys.argv) > 2 else None

XML = """
<node>
  <interface name="org.freedesktop.NetworkManager">
    <method name="GetDevices"><arg type="ao" direction="out"/></method>
    <method name="ActivateConnection">
      <arg type="o" direction="in"/><arg type="o" direction="in"/>
      <arg type="o" direction="in"/><arg type="o" direction="out"/>
    </method>
    <method name="AddAndActivateConnection">
      <arg type="a{sa{sv}}" direction="in"/><arg type="o" direction="in"/>
      <arg type="o" direction="in"/><arg type="o" direction="out"/>
      <arg type="o" direction="out"/>
    </method>
    <property name="WirelessEnabled" type="b" access="readwrite"/>
  </interface>
  <interface name="org.freedesktop.NetworkManager.Device">
    <property name="DeviceType" type="u" access="read"/>
    <property name="State" type="u" access="read"/>
    <property name="AvailableConnections" type="ao" access="read"/>
  </interface>
  <interface name="org.freedesktop.NetworkManager.Device.Wireless">
    <method name="RequestScan"><arg type="a{sv}" direction="in"/></method>
    <property name="AccessPoints" type="ao" access="read"/>
    <property name="ActiveAccessPoint" type="o" access="read"/>
    <signal name="AccessPointAdded"><arg type="o"/></signal>
    <signal name="AccessPointRemoved"><arg type="o"/></signal>
  </interface>
  <interface name="org.freedesktop.NetworkManager.AccessPoint">
    <property name="Ssid" type="ay" access="read"/>
    <property name="Strength" type="y" access="read"/>
    <property name="Flags" type="u" access="read"/>
    <property name="WpaFlags" type="u" access="read"/>
    <property name="RsnFlags" type="u" access="read"/>
    <property name="Mode" type="u" access="read"/>
  </interface>
  <interface name="org.freedesktop.NetworkManager.Settings.Connection">
    <method name="GetSettings"><arg type="a{sa{sv}}" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.ObjectManager">
    <method name="GetManagedObjects">
      <arg type="a{oa{sa{sv}}}" direction="out"/>
    </method>
  </interface>
  <interface name="org.bluez.Adapter1">
    <property name="Powered" type="b" access="readwrite"/>
  </interface>
  <interface name="org.bluez.Device1">
    <method name="Connect"/>
    <method name="Disconnect"/>
    <property name="Alias" type="s" access="read"/>
    <property name="Icon" type="s" access="read"/>
    <property name="Paired" type="b" access="read"/>
    <property name="Trusted" type="b" access="read"/>
    <property name="Connected" type="b" access="read"/>
  </interface>
  <interface name="org.freedesktop.UPower.PowerProfiles">
    <property name="ActiveProfile" type="s" access="readwrite"/>
    <property name="Profiles" type="aa{sv}" access="read"/>
    <property name="PerformanceDegraded" type="s" access="read"/>
  </interface>
  <interface name="org.freedesktop.login1.Session">
    <method name="SetBrightness">
      <arg type="s" direction="in"/><arg type="s" direction="in"/>
      <arg type="u" direction="in"/>
    </method>
    <method name="Terminate"/>
  </interface>
  <interface name="org.freedesktop.PolicyKit1.Authority">
    <method name="RegisterAuthenticationAgent">
      <arg type="(sa{sv})" direction="in"/><arg type="s" direction="in"/>
      <arg type="s" direction="in"/>
    </method>
  </interface>
  <interface name="org.gnome.DisplayManager.Manager">
    <property name="Version" type="s" access="read"/>
  </interface>
  <interface name="org.freedesktop.login1.Manager">
    <method name="Suspend"><arg type="b" direction="in"/></method>
    <method name="Reboot"><arg type="b" direction="in"/></method>
    <method name="PowerOff"><arg type="b" direction="in"/></method>
    <signal name="PrepareForSleep"><arg type="b"/></signal>
  </interface>
</node>
"""
NODE = Gio.DBusNodeInfo.new_for_xml(XML)

NM = "/org/freedesktop/NetworkManager"
DEV_ETH = NM + "/Devices/1"
DEV_WIFI = NM + "/Devices/2"
APS = [
    # path suffix, ssid, strength, flags, rsn flags
    (1, "Roost Home", 70, 1, 0x188),
    (2, "Roost Cafe", 85, 0, 0),
    (3, "Roost Office", 60, 1, 0x288),
    (4, "", 90, 0, 0),
]
AP_PATH = NM + "/AccessPoint/{}"
HOME_CONN = NM + "/Settings/1"
WIRELESS = "org.freedesktop.NetworkManager.Device.Wireless"
HCI = "/org/bluez/hci0"
BT_DEVICES = [
    # path suffix, alias, icon, paired, connected
    ("dev_AA_AA_AA_AA_AA_01", "Headphones", "audio-headphones", True, False),
    ("dev_AA_AA_AA_AA_AA_02", "Keyboard", "input-keyboard", True, True),
    ("dev_AA_AA_AA_AA_AA_03", "Stranger", "phone", False, False),
]
PPD = "/org/freedesktop/UPower/PowerProfiles"
SESSION = "/org/freedesktop/login1/session/auto"

# (path, interface) -> {property: GLib.Variant}
state = {
    (NM, "org.freedesktop.NetworkManager"): {"WirelessEnabled": GLib.Variant("b", True)},
    (DEV_ETH, "org.freedesktop.NetworkManager.Device"): {
        "DeviceType": GLib.Variant("u", 1), "State": GLib.Variant("u", 100),
        "AvailableConnections": GLib.Variant("ao", [])},
    (DEV_WIFI, "org.freedesktop.NetworkManager.Device"): {
        "DeviceType": GLib.Variant("u", 2), "State": GLib.Variant("u", 30),
        "AvailableConnections": GLib.Variant("ao", [HOME_CONN])},
    (DEV_WIFI, WIRELESS): {
        "AccessPoints": GLib.Variant("ao", [AP_PATH.format(n) for n, *_ in APS]),
        "ActiveAccessPoint": GLib.Variant("o", "/"),
    },
    **{(AP_PATH.format(n), "org.freedesktop.NetworkManager.AccessPoint"): {
        "Ssid": GLib.Variant("ay", ssid.encode()),
        "Strength": GLib.Variant("y", strength),
        "Flags": GLib.Variant("u", flags),
        "WpaFlags": GLib.Variant("u", 0),
        "RsnFlags": GLib.Variant("u", rsn),
        "Mode": GLib.Variant("u", 2),
    } for n, ssid, strength, flags, rsn in APS},
    (HCI, "org.bluez.Adapter1"): {"Powered": GLib.Variant("b", True)},
    **{(f"{HCI}/{d}", "org.bluez.Device1"): {
        "Alias": GLib.Variant("s", alias), "Icon": GLib.Variant("s", icon),
        "Paired": GLib.Variant("b", paired), "Trusted": GLib.Variant("b", False),
        "Connected": GLib.Variant("b", connected),
    } for d, alias, icon, paired, connected in BT_DEVICES},
    ("/org/gnome/DisplayManager/Manager", "org.gnome.DisplayManager.Manager"): {
        "Version": GLib.Variant("s", "51.0"),
    },
    (PPD, "org.freedesktop.UPower.PowerProfiles"): {
        "ActiveProfile": GLib.Variant("s", "balanced"),
        "Profiles": GLib.Variant("aa{sv}", [
            {"Profile": GLib.Variant("s", p), "Driver": GLib.Variant("s", "placeholder")}
            for p in ("power-saver", "balanced", "performance")]),
        "PerformanceDegraded": GLib.Variant("s", ""),
    },
}

conn = Gio.DBusConnection.new_for_address_sync(
    os.environ["DBUS_SYSTEM_BUS_ADDRESS"],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
    | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)


def method_call(c, sender, path, iface, method, params, invocation):
    if method == "GetDevices":
        invocation.return_value(GLib.Variant("(ao)", ([DEV_ETH, DEV_WIFI],)))
    elif method in ("RequestScan", "ActivateConnection", "AddAndActivateConnection"):
        log = os.environ.get("ROOST_STUB_NM_LOG")
        args = params.unpack()
        if log:
            with open(log, "a") as fh:
                fh.write(f"{method} {' '.join(str(a) for a in args if isinstance(a, str))}\n")
        if method == "RequestScan":
            invocation.return_value(None)
            return
        # The access point the activation is for becomes the active one.
        ap = args[2] if method == "AddAndActivateConnection" else AP_PATH.format(1)
        if method == "ActivateConnection" and args[0] != HOME_CONN:
            ap = "/"
        set_property(c, None, DEV_WIFI, WIRELESS, "ActiveAccessPoint", GLib.Variant("o", ap))
        active = NM + "/ActiveConnection/1"
        if method == "ActivateConnection":
            invocation.return_value(GLib.Variant("(o)", (active,)))
        else:
            invocation.return_value(GLib.Variant("(oo)", (NM + "/Settings/2", active)))
    elif method == "GetSettings":
        invocation.return_value(GLib.Variant("(a{sa{sv}})", ({
            "connection": {"id": GLib.Variant("s", "Roost Home"),
                           "type": GLib.Variant("s", "802-11-wireless")},
            "802-11-wireless": {"ssid": GLib.Variant("ay", b"Roost Home")},
        },)))
    elif method == "GetManagedObjects":
        objects = {HCI: {"org.bluez.Adapter1": state[(HCI, "org.bluez.Adapter1")]}}
        for d, *_ in BT_DEVICES:
            path = f"{HCI}/{d}"
            objects[path] = {"org.bluez.Device1": state[(path, "org.bluez.Device1")]}
        invocation.return_value(GLib.Variant("(a{oa{sa{sv}}})", (objects,)))
    elif method in ("Connect", "Disconnect"):
        log = os.environ.get("ROOST_STUB_BT_LOG")
        if log:
            with open(log, "a") as fh:
                fh.write(f"{method} {path}\n")
        set_property(c, None, path, "org.bluez.Device1", "Connected",
                     GLib.Variant("b", method == "Connect"))
        invocation.return_value(None)
    elif method in ("Suspend", "Reboot", "PowerOff", "Terminate"):
        if method == "Suspend":
            c.emit_signal(None, "/org/freedesktop/login1", "org.freedesktop.login1.Manager",
                          "PrepareForSleep", GLib.Variant("(b)", (True,)))
        if POWER_LOG:
            with open(POWER_LOG, "a") as fh:
                fh.write(method + "\n")
        invocation.return_value(None)
    elif method == "RegisterAuthenticationAgent":
        _, _, agent_path = params.unpack()
        log = os.environ.get("ROOST_STUB_POLKIT_LOG")
        if log:
            with open(log, "w") as fh:
                fh.write(f"{sender} {agent_path}\n")
        invocation.return_value(None)
    elif method == "SetBrightness":
        _, _, value = params.unpack()
        # Atomically, as the kernel's attribute never reads half-written:
        # a reader racing a truncate-then-write would see it empty.
        path = os.path.join(BACKLIGHT, "brightness")
        with open(path + ".tmp", "w") as fh:
            fh.write(f"{value}\n")
        os.replace(path + ".tmp", path)
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


SERVICES = {
    "nm": (["org.freedesktop.NetworkManager"], [
        (NM, "org.freedesktop.NetworkManager"),
        (DEV_ETH, "org.freedesktop.NetworkManager.Device"),
        (DEV_WIFI, "org.freedesktop.NetworkManager.Device"),
        (DEV_WIFI, WIRELESS),
        (HOME_CONN, "org.freedesktop.NetworkManager.Settings.Connection"),
        *[(AP_PATH.format(n), "org.freedesktop.NetworkManager.AccessPoint") for n, *_ in APS],
    ]),
    "bluez": (["org.bluez"], [
        ("/", "org.freedesktop.DBus.ObjectManager"),
        (HCI, "org.bluez.Adapter1"),
        *[(f"{HCI}/{d}", "org.bluez.Device1") for d, *_ in BT_DEVICES],
    ]),
    "ppd": (["org.freedesktop.UPower.PowerProfiles"], [
        (PPD, "org.freedesktop.UPower.PowerProfiles"),
    ]),
    "gdm": (["org.gnome.DisplayManager"], [
        ("/org/gnome/DisplayManager/Manager", "org.gnome.DisplayManager.Manager"),
    ]),
    "polkit": (["org.freedesktop.PolicyKit1"], [
        ("/org/freedesktop/PolicyKit1/Authority", "org.freedesktop.PolicyKit1.Authority"),
    ]),
    "logind": (["org.freedesktop.login1"], [
        (SESSION, "org.freedesktop.login1.Session"),
        ("/org/freedesktop/login1", "org.freedesktop.login1.Manager"),
    ]),
}
wanted = os.environ.get(
    "ROOST_STUB_SERVICES", ",".join(k for k in SERVICES if k != "gdm")).split(",")
names = []
for key in wanted:
    if key not in SERVICES:
        continue
    bus_names, objects = SERVICES[key]
    names += bus_names
    for path, iface in objects:
        serve(path, iface)

for name in names:
    conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                   "RequestName", GLib.Variant("(su)", (name, 4)), None,
                   Gio.DBusCallFlags.NONE, -1, None)

print("roost-service-stubs: ready", flush=True)
GLib.MainLoop().run()
