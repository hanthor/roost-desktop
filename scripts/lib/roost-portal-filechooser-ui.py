#!/usr/bin/python3
"""Drive the actual Nautilus file-picker dialog, without fabricating a response."""
import json
from pathlib import Path
import subprocess
import sys
import time
import pyatspi

out = Path(sys.argv[1])
request = json.loads(out.with_suffix(".waiting.json").read_text())
def walk(acc, nodes, controls):
    try:
        showing = acc.getState().contains(pyatspi.STATE_SHOWING)
        nodes.append({"role": acc.getRoleName(), "name": acc.name, "showing": showing})
        if showing:
            controls.append(acc)
        for i in range(acc.childCount):
            walk(acc.getChildAtIndex(i), nodes, controls)
    except Exception:
        pass
def dialog():
    nodes, controls = [], []
    desktop = pyatspi.Registry.getDesktop(0)
    for i in range(desktop.childCount):
        app = desktop.getChildAtIndex(i)
        frames, unused = [], []
        walk(app, frames, unused)
        if not any(node["showing"] and node["name"] == request["title"] for node in frames):
            continue
        for j in range(app.childCount):
            frame = app.getChildAtIndex(j)
            if frame.name == request["title"]:
                walk(frame, nodes, controls)
                return nodes, controls
    return nodes, controls
end = time.monotonic() + 30
located = False
while time.monotonic() < end:
    nodes, controls = dialog()
    if controls:
        out.with_suffix(".a11y.json").write_text(json.dumps(nodes, indent=2))
        subprocess.run(["scrot", str(out.with_suffix(".png"))], check=True)
        if request["method"] == "OpenFile" and request["decision"] == "grant" and not located:
            # The dialog is the focused new window. Ctrl+L invokes Nautilus's
            # real location entry; keyboard input selects the fixture file.
            subprocess.run(["xdotool", "key", "--clearmodifiers", "ctrl+l"], check=True)
            time.sleep(.2)
            nodes, controls = dialog()
            editable = []
            for control in controls:
                try:
                    if control.getState().contains(pyatspi.STATE_FOCUSED):
                        editable.append(control.queryEditableText())
                except Exception:
                    pass
            if len(editable) != 1 or not editable[0].setTextContents(request["path"]):
                raise RuntimeError("actual focused Nautilus location entry was not editable")
            subprocess.run(["xdotool", "key", "--clearmodifiers", "Return"], check=True)
            located = True
            time.sleep(.2)
            continue
        name = "Cancel" if request["decision"] == "cancel" else "Choose proof file"
        for control in controls:
            if control.getRoleName() in ("push button", "button") and control.name == name and control.getState().contains(pyatspi.STATE_SENSITIVE):
                if not control.queryAction().doAction(0):
                    raise RuntimeError("Nautilus picker action was refused")
                print("actual Nautilus FileChooser action: " + name)
                sys.exit(0)
    elif located and out.with_suffix(".response.json").exists():
        print("actual Nautilus keyboard location activation completed")
        sys.exit(0)
    time.sleep(.1)
nodes, controls = dialog()
out.with_suffix(".a11y.json").write_text(json.dumps(nodes, indent=2))
raise RuntimeError("actual Nautilus FileChooser did not become actionable")
