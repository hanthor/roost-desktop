#!/usr/bin/python3
"""Drive the actual Nautilus file-picker dialog, without fabricating a response."""
import json
from pathlib import Path
import subprocess
import sys
import time
import pyatspi
from gi.repository import GLib

out = Path(sys.argv[1])
request = json.loads(out.with_suffix(".waiting.json").read_text())
def walk(acc, nodes, controls):
    try:
        acc.clear_cache()
        state = acc.getState()
        showing = state.contains(pyatspi.STATE_SHOWING)
        nodes.append({"role": acc.getRoleName(), "name": acc.name, "showing": showing,
                      "selected": state.contains(pyatspi.STATE_SELECTED),
                      "focused": state.contains(pyatspi.STATE_FOCUSED),
                      "sensitive": state.contains(pyatspi.STATE_SENSITIVE)})
        if showing:
            controls.append(acc)
        for i in range(acc.childCount):
            walk(acc.getChildAtIndex(i), nodes, controls)
    except Exception:
        pass
def dialog():
    while GLib.MainContext.default().pending():
        GLib.MainContext.default().iteration(False)
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
parent_checked = request.get("parent") is None
while time.monotonic() < end:
    nodes, controls = dialog()
    if controls:
        if not parent_checked:
            relationship_end = min(end, time.monotonic() + 5)
            while time.monotonic() < relationship_end:
                current = json.loads(Path("/out/compositor-state.json").read_text())
                parent_id = request["parent"]["id"]
                parents = [w for w in current["windows"] if w["id"] == parent_id]
                children = [w for w in current["windows"] if w.get("parent_window_id") == parent_id
                            and w.get("app_id") == "org.gnome.Nautilus"]
                if len(parents) == len(children) == 1:
                    child, parent = children[0], parents[0]
                    if child.get("application_window_id") == parent_id and current["focused"] == child["id"]:
                        cx, cy, cw, ch = child["rect"]
                        px, py, pw, ph = parent["rect"]
                        if abs(2*cx + cw - (2*px + pw)) <= 1 and abs(2*cy + ch - (2*py + ph)) <= 1:
                            out.with_suffix(".x11-parent.json").write_text(json.dumps(current, indent=2))
                            parent_checked = True
                            break
                time.sleep(.05)
            else:
                raise RuntimeError("Actual Nautilus dialog did not inherit its live X11 parent, centered placement and focus")
        out.with_suffix(".a11y.json").write_text(json.dumps(nodes, indent=2))
        subprocess.run(["scrot", str(out.with_suffix(".png"))], check=True)
        if request["method"] == "OpenFile" and request["decision"] == "grant" and not located:
            # Click the actual expected file cell once. WINDOW coordinates
            # plus the compositor-known focused-surface origin avoid
            # assuming Wayland accessibility exposes global coordinates.
            filename = Path(request["path"]).name
            cells = [control for control in controls
                     if control.getRoleName() == "table cell"
                     and control.name in (filename, filename + ". File")]
            if len(cells) != 1:
                raise RuntimeError("actual Nautilus fixture cell is not unique")
            rect = cells[0].queryComponent().getExtents(pyatspi.WINDOW_COORDS)
            state = json.loads((out.parent / "compositor-state.json").read_text())
            if state.get("focused_app_id") != "org.gnome.Nautilus":
                raise RuntimeError("actual Nautilus dialog does not own focus")
            fx, fy, fw, fh = state["focused_rect"]
            if (rect.width <= 0 or rect.height <= 0 or rect.x < 0 or rect.y < 0
                    or rect.x + rect.width > fw or rect.y + rect.height > fh):
                raise RuntimeError("file cell bounds are outside the focused Nautilus dialog")
            windows = subprocess.check_output(
                ["xdotool", "search", "--onlyvisible", "--name", "^Smithay"], text=True).split()
            if len(windows) != 1:
                raise RuntimeError("expected one nested compositor host window")
            geometry = dict(line.split("=", 1) for line in subprocess.check_output(
                ["xdotool", "getwindowgeometry", "--shell", windows[0]], text=True).splitlines())
            x, y = fx + rect.x + rect.width // 2, fy + rect.y + rect.height // 2
            if not (0 <= x < int(geometry["WIDTH"]) and 0 <= y < int(geometry["HEIGHT"])):
                raise RuntimeError("file click is outside the nested compositor host")
            out.with_suffix(".selection-click.json").write_text(json.dumps(
                {"host_window": windows[0], "focused_rect": state["focused_rect"],
                 "cell_bounds": [rect.x, rect.y, rect.width, rect.height],
                 "click": [x, y], "file": filename}, indent=2))
            subprocess.run(["xdotool", "mousemove", "--window", windows[0],
                            str(x), str(y), "click", "1"], check=True)
            # Observe the GTK/AT-SPI update without replaying the click or
            # accepting a stale cached selection from before the action.
            selection_end = min(end, time.monotonic() + 5)
            while time.monotonic() < selection_end:
                nodes, controls = dialog()
                out.with_suffix(".selection-a11y.json").write_text(json.dumps(nodes, indent=2))
                if out.with_suffix(".response.json").exists():
                    raise RuntimeError("file selection responded before explicit acceptance")
                selected = [control for control in controls
                            if control.getRoleName() == "table cell"
                            and control.getState().contains(pyatspi.STATE_SELECTED)]
                if len(selected) == 1 and selected[0].name in (filename, filename + ". File"):
                    break
                time.sleep(.05)
            else:
                raise RuntimeError("actual Nautilus selection did not identify the fixture file")
            print("actual Nautilus selected file: " + selected[0].name)
            located = True
            time.sleep(.2)
            continue
        # GNOME 51 Nautilus cancels through its header-bar Close button.
        # Invoke that real widget; the client still requires response 1 and
        # no returned URI, rather than treating dismissal as proof itself.
        name = "Close" if request["decision"] == "cancel" else "Choose proof file"
        for control in controls:
            if control.getRoleName() in ("push button", "button") and control.name == name and control.getState().contains(pyatspi.STATE_SENSITIVE):
                if not control.queryAction().doAction(0):
                    raise RuntimeError("Nautilus picker action was refused")
                print("actual Nautilus FileChooser action: " + name)
                sys.exit(0)
    time.sleep(.1)
nodes, controls = dialog()
out.with_suffix(".a11y.json").write_text(json.dumps(nodes, indent=2))
raise RuntimeError("actual Nautilus FileChooser did not become actionable")
