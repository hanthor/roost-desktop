#!/usr/bin/python3
"""Activate the actual GNOME portal consent picker through AT-SPI."""
import json
import sys
import subprocess
from pathlib import Path
import time
import pyatspi
out, decision = sys.argv[1:]
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
        buttons = [a for a in controls if a.getRoleName() in ("push button", "button") and a.name in (("Cancel", "Deny") if decision == "cancel" else ("Share", "Allow"))]
        if not buttons: continue
        with open(out, "w") as f: json.dump(nodes, f, indent=2)
        subprocess.run(["scrot", str(Path(out).with_suffix(".png"))], check=True)
        button = buttons[0]
        if decision != "cancel" and not button.getState().contains(pyatspi.STATE_SENSITIVE):
            for control in controls:
                if control.getRoleName() in ("toggle button", "radio button") and control.name not in ("Windows", "Screens"):
                    try:
                        control.queryAction().doAction(0)
                        time.sleep(.2)
                        break
                    except Exception: pass
        if button.getState().contains(pyatspi.STATE_SENSITIVE):
            button_name = button.name
            button.queryAction().doAction(0)
            print("actual portal picker: " + button_name)
            sys.exit(0)
    time.sleep(.1)
nodes, controls = [], []
walk(pyatspi.Registry.getDesktop(0), nodes, controls)
Path(out).write_text(json.dumps(nodes, indent=2))
subprocess.run(["scrot", str(Path(out).with_suffix(".png"))], check=True)
raise RuntimeError("actual GNOME portal consent picker did not become actionable")
