#!/usr/bin/python3
"""Observe real Introspect properties/signals during live GNOME motion changes."""
import json
import sys
import time
from pathlib import Path

from gi.repository import Gio, GLib

state_path, report_path = map(Path, sys.argv[1:])
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
name = "org.gnome.Shell.Introspect"
path = "/org/gnome/Shell/Introspect"
signals = []
results = []


def changed(_bus, _sender, _path, _interface, _signal, parameters, _data):
    interface, values, invalidated = parameters.unpack()
    if interface == name and "AnimationsEnabled" in values:
        if "AnimationsEnabled" in invalidated:
            raise RuntimeError("motion property both changed and invalidated")
        signals.append(values["AnimationsEnabled"])


subscription = bus.signal_subscribe(
    name, "org.freedesktop.DBus.Properties", "PropertiesChanged", path,
    name, Gio.DBusSignalFlags.NONE, changed, None)


def property_value():
    # Also provides a round trip after installing the subscription.
    return bus.call_sync(
        name, path, "org.freedesktop.DBus.Properties", "Get",
        GLib.Variant("(ss)", (name, "AnimationsEnabled")),
        GLib.VariantType.new("(v)"), Gio.DBusCallFlags.NONE, 5000, None
    ).unpack()[0]


def observe(label, expected, signal_count, policy):
    deadline = time.monotonic() + 10
    context = GLib.MainContext.default()
    while time.monotonic() < deadline:
        while context.pending():
            context.iteration(False)
        actual = property_value()
        state = json.loads(state_path.read_text())
        compositor = state["animations_enabled"]
        level = state.get("motion_policy")
        if (actual == expected and compositor == expected and level == policy
                and len(signals) == signal_count):
            results.append({"step": label, "property": actual,
                            "compositor": compositor, "motion_policy": level,
                            "signals": list(signals)})
            return
        time.sleep(0.02)
    raise RuntimeError(f"{label}: property={actual}, compositor={compositor}, "
                       f"motion_policy={level}, signals={signals}")


interface = Gio.Settings.new("org.gnome.desktop.interface")
a11y = Gio.Settings.new("org.gnome.desktop.a11y.interface")
try:
    observe("initial", True, 0, "full")
    if not interface.set_boolean("enable-animations", False):
        raise RuntimeError("animation setting is not writable")
    observe("explicit disable", False, 1, "off")
    interface.set_boolean("enable-animations", True)
    observe("explicit enable", True, 2, "full")
    if not a11y.set_string("reduced-motion", "reduce"):
        raise RuntimeError("reduced motion setting is not writable")
    # GNOME 51: Reduced Motion keeps fades, so AnimationsEnabled stays true.
    observe("reduced motion keeps animations enabled", True, 2, "fade-only")
    interface.set_boolean("enable-animations", False)
    a11y.reset("reduced-motion")
    observe("reset preserves explicit disable", False, 3, "off")
    interface.set_boolean("enable-animations", True)
    observe("motion restored", True, 4, "full")
    if signals != [False, True, False, True]:
        raise RuntimeError(f"unexpected motion notifications: {signals}")
    report_path.write_text(json.dumps(results, indent=2) + "\n")
    print("Introspect motion: live property, four enable-animations notifications and compositor agree")
finally:
    a11y.reset("reduced-motion")
    interface.set_boolean("enable-animations", True)
    Gio.Settings.sync()
    bus.signal_unsubscribe(subscription)
