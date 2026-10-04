#!/usr/bin/python3
"""System-service stubs for quick-settings proofs (#55).

Usage: roost-service-stubs.py BACKLIGHT_DIR [POWER_LOG]

Serves, on the bus at $DBUS_SYSTEM_BUS_ADDRESS (a private bus the proof
starts), just enough of each daemon behind GNOME 51's quick settings:

- NetworkManager: WirelessEnabled (rw), Connectivity (full) and
  PrimaryConnection, GetDevices with one ethernet and one wifi device.
  The ethernet device is up on its one saved profile ("Wired connection
  1"); Device.Disconnect takes it down and ActivateConnection brings it
  back, both appended to $ROOST_STUB_NM_LOG. The wifi device sees four access
  points (a saved WPA2 "Roost Home", an open "Roost Cafe", an 802.1X
  "Roost Office" and a hidden one); RequestScan, ActivateConnection and
  AddAndActivateConnection are appended to $ROOST_STUB_NM_LOG, and an
  activation makes its access point the active one. A secret agent
  registered with the AgentManager is asked for an unsaved secured
  network's password (logged as "Secrets psk=..." or "Secrets error").
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
    <signal name="DeviceAdded"><arg type="o"/></signal>
    <signal name="DeviceRemoved"><arg type="o"/></signal>
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
    <property name="NetworkingEnabled" type="b" access="read"/>
    <property name="Connectivity" type="u" access="read"/>
    <property name="PrimaryConnection" type="o" access="read"/>
  </interface>
  <interface name="org.roost.Proof.Network">
    <method name="RequestSecrets"><arg type="s" direction="in"/><arg type="u" direction="in"/></method>
    <method name="DeleteSecrets"><arg type="s" direction="in"/></method>
    <method name="SecondWired"><arg type="b" direction="in"/></method>
  </interface>
  <interface name="org.freedesktop.NetworkManager.Connection.Active">
    <property name="Connection" type="o" access="read"/>
    <property name="State" type="u" access="read"/>
  </interface>
  <interface name="org.freedesktop.NetworkManager.AgentManager">
    <method name="Register"><arg type="s" direction="in"/></method>
  </interface>
  <interface name="org.freedesktop.NetworkManager.Device">
    <property name="DeviceType" type="u" access="read"/>
    <property name="Interface" type="s" access="read"/>
    <property name="State" type="u" access="read"/>
    <property name="AvailableConnections" type="ao" access="read"/>
    <property name="ActiveConnection" type="o" access="read"/>
    <method name="Disconnect"/>
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
  <interface name="org.freedesktop.UPower.Device">
    <property name="IsPresent" type="b" access="readwrite"/>
    <property name="Type" type="u" access="read"/>
    <property name="State" type="u" access="readwrite"/>
    <property name="Percentage" type="d" access="readwrite"/>
    <property name="TimeToEmpty" type="x" access="readwrite"/>
    <property name="TimeToFull" type="x" access="readwrite"/>
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
DEV_ETH2 = NM + "/Devices/3"
second_wired = False
APS = [
    # path suffix, ssid, strength, flags, rsn flags
    (1, "Roost Home", 70, 1, 0x188),
    (2, "Roost Cafe", 85, 0, 0),
    (3, "Roost Office", 60, 1, 0x288),
    (4, "", 90, 0, 0),
    (5, "Roost Guest", 50, 1, 0x188),
]
AP_PATH = NM + "/AccessPoint/{}"
HOME_CONN = NM + "/Settings/1"
WIRED_CONN = NM + "/Settings/3"
WIRED_ACTIVE = NM + "/ActiveConnection/3"
DEVICE = "org.freedesktop.NetworkManager.Device"
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
    (NM, "org.freedesktop.NetworkManager"): {
        "WirelessEnabled": GLib.Variant("b", True),
        "NetworkingEnabled": GLib.Variant("b", True),
        "Connectivity": GLib.Variant("u", 4),
        "PrimaryConnection": GLib.Variant("o", WIRED_ACTIVE)},
    (DEV_ETH, "org.freedesktop.NetworkManager.Device"): {
        "DeviceType": GLib.Variant("u", 1), "Interface": GLib.Variant("s", "enp1s0"), "State": GLib.Variant("u", 100),
        "AvailableConnections": GLib.Variant("ao", [WIRED_CONN]),
        "ActiveConnection": GLib.Variant("o", WIRED_ACTIVE)},
    (DEV_ETH2, DEVICE): {
        "DeviceType": GLib.Variant("u", 1), "Interface": GLib.Variant("s", "enp2s0"), "State": GLib.Variant("u", 30),
        "AvailableConnections": GLib.Variant("ao", []), "ActiveConnection": GLib.Variant("o", "/")},
    (WIRED_ACTIVE, "org.freedesktop.NetworkManager.Connection.Active"): {
        "Connection": GLib.Variant("o", WIRED_CONN), "State": GLib.Variant("u", 2)},
    (DEV_WIFI, "org.freedesktop.NetworkManager.Device"): {
        "DeviceType": GLib.Variant("u", 2), "Interface": GLib.Variant("s", "wlp1s0"), "State": GLib.Variant("u", 30),
        "AvailableConnections": GLib.Variant("ao", [HOME_CONN]),
        "ActiveConnection": GLib.Variant("o", "/")},
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
    ("/org/freedesktop/UPower/devices/DisplayDevice", "org.freedesktop.UPower.Device"): {
        "IsPresent": GLib.Variant("b", False), "Type": GLib.Variant("u", 2),
        "State": GLib.Variant("u", 2), "Percentage": GLib.Variant("d", 37.0),
        "TimeToEmpty": GLib.Variant("x", 5400), "TimeToFull": GLib.Variant("x", 1800),
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


agents = []


def secured_ap(path):
    props = state.get((path, "org.freedesktop.NetworkManager.AccessPoint"))
    return bool(props) and props["Flags"].unpack() & 1


def proof_connection(kind):
    """Real NM agent wire formats; flags1 select user/agent-owned storage."""
    uuid = {"enterprise": "00000000-0000-4000-8000-000000000211",
            "tls": "00000000-0000-4000-8000-000000000212",
            "vpn": "00000000-0000-4000-8000-000000000213"}[kind]
    connection = {"connection": {"uuid": GLib.Variant("s", uuid),
                  "id": GLib.Variant("s", "Roost Office" if kind != "vpn" else "Roost VPN"),
                  "type": GLib.Variant("s", "vpn" if kind == "vpn" else "802-11-wireless")}}
    if kind == "vpn":
        connection["vpn"] = {"service-type": GLib.Variant("s", "org.roost.Proof.VPN"),
                             "data": GLib.Variant("a{ss}", {"password-flags": "1"}),
                             "secrets": GLib.Variant("a{ss}", {})}
        return connection, "vpn", ["password"]
    connection["802-11-wireless"] = {"ssid": GLib.Variant("ay", b"Roost Office")}
    connection["802-11-wireless-security"] = {"key-mgmt": GLib.Variant("s", "wpa-eap")}
    connection["802-1x"] = {"identity": GLib.Variant("s", "alice"),
                            "eap": GLib.Variant("as", ["tls" if kind == "tls" else "peap"]),
                            "password-flags": GLib.Variant("u", 1),
                            "private-key-password-flags": GLib.Variant("u", 1)}
    return connection, "802-1x", (["private-key-password"] if kind == "tls" else ["identity", "password"])


def proof_request(c, kind, flags, delete=False):
    connection, setting, hints = proof_connection(kind)
    def answered(bus, result):
        try:
            reply = bus.call_finish(result)
            if delete:
                line = f"ProofSecrets {kind} deleted"
            else:
                values = reply.unpack()[0][setting]
                if setting == "vpn":
                    values = values["secrets"]
                line = f"ProofSecrets {kind} flags={flags} " + " ".join(f"{k}={v}" for k, v in sorted(values.items()))
        except GLib.Error as error:
            line = f"ProofSecrets {kind} flags={flags} error=" + str(Gio.DBusError.get_remote_error(error))
        log = os.environ.get("ROOST_STUB_NM_LOG")
        if log:
            with open(log, "a") as fh:
                fh.write(line + "\n")
    for agent in agents[-1:]:
        c.call(agent, "/org/freedesktop/NetworkManager/SecretAgent", "org.freedesktop.NetworkManager.SecretAgent",
               "DeleteSecrets" if delete else "GetSecrets",
               GLib.Variant("(a{sa{sv}}o)", (connection, NM + "/Settings/211")) if delete else
               GLib.Variant("(a{sa{sv}}osasu)", (connection, NM + "/Settings/211", setting, hints, flags)),
               None, Gio.DBusCallFlags.NONE, -1, None, answered)


def method_call(c, sender, path, iface, method, params, invocation):
    global second_wired
    if iface == "org.roost.Proof.Network":
        if method == "SecondWired":
            second_wired = params.unpack()[0]
            c.emit_signal(None, NM, "org.freedesktop.NetworkManager", "DeviceAdded" if second_wired else "DeviceRemoved", GLib.Variant("(o)", (DEV_ETH2,)))
        else:
            args = params.unpack()
            proof_request(c, args[0], args[1] if len(args) > 1 else 0, method == "DeleteSecrets")
        invocation.return_value(None)
    elif method == "GetDevices":
        invocation.return_value(GLib.Variant("(ao)", ([DEV_ETH, DEV_WIFI] + ([DEV_ETH2] if second_wired else []),)))
    elif method == "Register":
        agents.append(sender)
        invocation.return_value(None)
    elif method == "AddAndActivateConnection" and secured_ap(params.unpack()[2]):
        # NetworkManager asks the agent for the password first.
        ap_path = params.unpack()[2]
        ssid = state[(ap_path, "org.freedesktop.NetworkManager.AccessPoint")]["Ssid"].unpack()
        log = os.environ.get("ROOST_STUB_NM_LOG")
        connection = {
            "connection": {"id": GLib.Variant("s", bytes(ssid).decode()),
                           "type": GLib.Variant("s", "802-11-wireless")},
            "802-11-wireless": {"ssid": GLib.Variant("ay", bytes(ssid))},
            "802-11-wireless-security": {"key-mgmt": GLib.Variant("s", "wpa-psk")},
        }

        def answered(conn, res):
            try:
                reply = conn.call_finish(res).unpack()[0]
                line = "Secrets psk=" + reply["802-11-wireless-security"]["psk"]
            except GLib.Error as e:
                line = "Secrets error " + Gio.DBusError.get_remote_error(e)
            if log:
                with open(log, "a") as fh:
                    fh.write(line + "\n")
        if log:
            with open(log, "a") as fh:
                fh.write(f"AddAndActivateConnection {ap_path}\n")
        for agent in agents[-1:]:
            c.call(agent, "/org/freedesktop/NetworkManager/SecretAgent",
                   "org.freedesktop.NetworkManager.SecretAgent", "GetSecrets",
                   GLib.Variant("(a{sa{sv}}osasu)", (connection, NM + "/Settings/2",
                                "802-11-wireless-security", [], 1)),
                   None, Gio.DBusCallFlags.NONE, -1, None, answered)
        invocation.return_value(GLib.Variant("(oo)", (NM + "/Settings/2", NM + "/ActiveConnection/2")))
    elif method == "Disconnect" and path == DEV_ETH:
        log = os.environ.get("ROOST_STUB_NM_LOG")
        if log:
            with open(log, "a") as fh:
                fh.write(f"Disconnect {path}\n")
        wired_up(c, False)
        invocation.return_value(None)
    elif method in ("ActivateConnection", "AddAndActivateConnection") \
            and params.unpack()[1] == DEV_ETH:
        log = os.environ.get("ROOST_STUB_NM_LOG")
        if log:
            with open(log, "a") as fh:
                fh.write(f"{method} {' '.join(a for a in params.unpack() if isinstance(a, str))}\n")
        wired_up(c, True)
        if method == "ActivateConnection":
            invocation.return_value(GLib.Variant("(o)", (WIRED_ACTIVE,)))
        else:
            invocation.return_value(GLib.Variant("(oo)", (WIRED_CONN, WIRED_ACTIVE)))
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
    elif method == "GetSettings" and path == WIRED_CONN:
        invocation.return_value(GLib.Variant("(a{sa{sv}})", ({
            "connection": {"id": GLib.Variant("s", "Wired connection 1"),
                           "type": GLib.Variant("s", "802-3-ethernet"),
                           "timestamp": GLib.Variant("t", 1790000000)},
        },)))
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


def wired_up(c, up):
    """The ethernet device's profile goes up or down, as NetworkManager
    reports it: device state, active connection, primary connection."""
    set_property(c, None, DEV_ETH, DEVICE, "State", GLib.Variant("u", 100 if up else 30))
    set_property(c, None, DEV_ETH, DEVICE, "ActiveConnection",
                 GLib.Variant("o", WIRED_ACTIVE if up else "/"))
    set_property(c, None, NM, "org.freedesktop.NetworkManager", "PrimaryConnection",
                 GLib.Variant("o", WIRED_ACTIVE if up else "/"))


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
        (NM, "org.roost.Proof.Network"),
        (DEV_ETH2, DEVICE),
        (DEV_ETH, "org.freedesktop.NetworkManager.Device"),
        (DEV_WIFI, "org.freedesktop.NetworkManager.Device"),
        (DEV_WIFI, WIRELESS),
        (NM + "/AgentManager", "org.freedesktop.NetworkManager.AgentManager"),
        (HOME_CONN, "org.freedesktop.NetworkManager.Settings.Connection"),
        (WIRED_CONN, "org.freedesktop.NetworkManager.Settings.Connection"),
        (WIRED_ACTIVE, "org.freedesktop.NetworkManager.Connection.Active"),
        *[(AP_PATH.format(n), "org.freedesktop.NetworkManager.AccessPoint") for n, *_ in APS],
    ]),
    "bluez": (["org.bluez"], [
        ("/", "org.freedesktop.DBus.ObjectManager"),
        (HCI, "org.bluez.Adapter1"),
        *[(f"{HCI}/{d}", "org.bluez.Device1") for d, *_ in BT_DEVICES],
    ]),
    "upower": (["org.freedesktop.UPower"], [
        ("/org/freedesktop/UPower/devices/DisplayDevice", "org.freedesktop.UPower.Device"),
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
