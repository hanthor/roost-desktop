#!/usr/bin/python3
"""Activate the actual GNOME portal consent picker through AT-SPI."""
import json
import sys
import subprocess
from pathlib import Path
import time
import pyatspi
from gi.repository import GLib
out, decision = sys.argv[1:3]
remote = len(sys.argv) > 3 and sys.argv[3] == "remote"
def walk(acc, nodes, controls):
    try:
        showing = acc.getState().contains(pyatspi.STATE_SHOWING)
        nodes.append({"role": acc.getRoleName(), "name": acc.name, "showing": showing})
        if showing: controls.append(acc)
        for i in range(acc.childCount): walk(acc.getChildAtIndex(i), nodes, controls)
    except Exception:
        pass
deadline = time.monotonic() + 30
while time.monotonic() < deadline:
    desktop = pyatspi.Registry.getDesktop(0)
    for i in range(desktop.childCount):
        app = desktop.getChildAtIndex(i)
        if "portal" not in (app.name or "").lower(): continue
        nodes, controls = [], []
        walk(app, nodes, controls)
        buttons = [a for a in controls if a.getRoleName() in ("push button", "button") and a.name in (("Cancel",) if decision == "cancel" else ("Share", "Allow"))]
        if not buttons: continue
        with open(out, "w") as f: json.dump(nodes, f, indent=2)
        subprocess.run(["scrot", str(Path(out).with_suffix(".png"))], check=True)
        button = buttons[0]
        if remote and decision != "cancel":
            interaction = [control for control in controls
                           if control.getRoleName() == "switch"
                           and control.name == "Allow Remote Interaction"]
            if not interaction:
                raise RuntimeError("remote interaction consent switch missing")
            actionable = []
            for control in interaction:
                try:
                    if control.queryAction().nActions:
                        actionable.append(control)
                except Exception:
                    pass
            if not actionable:
                raise RuntimeError("remote interaction switch has no action")
            control = actionable[0]
            control.clear_cache()
            if not control.getState().contains(pyatspi.STATE_CHECKED):
                control.queryAction().doAction(0)
            enabled_deadline = time.monotonic() + 3
            while time.monotonic() < enabled_deadline:
                while GLib.MainContext.default().pending():
                    GLib.MainContext.default().iteration(False)
                control.clear_cache()
                if control.getState().contains(pyatspi.STATE_CHECKED):
                    break
                time.sleep(.05)
            else:
                raise RuntimeError("remote interaction consent was not enabled")
        if decision != "cancel" and not button.getState().contains(pyatspi.STATE_SENSITIVE):
            for control in controls:
                if control.getRoleName() in ("toggle button", "radio button") and control.name not in ("Windows", "Screens"):
                    try:
                        control.queryAction().doAction(0)
                        time.sleep(.2)
                        break
                    except Exception: pass
        if button.getState().contains(pyatspi.STATE_SENSITIVE):
            button.queryAction().doAction(0)
            print("actual portal picker: " + button.name)
            sys.exit(0)
    time.sleep(.1)
raise RuntimeError("actual GNOME portal consent picker did not become actionable")
