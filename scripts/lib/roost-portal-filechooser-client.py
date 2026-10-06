#!/usr/bin/python3
"""Exercise the installed GNOME/Nautilus FileChooser, retaining actual responses."""
import hashlib
import json
import os
from pathlib import Path
import sys
import subprocess
import time
from urllib.parse import unquote, urlparse
from gi.repository import Gio, GLib
import gi

out, method, decision = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
parent_mode = sys.argv[4] if len(sys.argv) > 4 else "none"
if parent_mode not in ("none", "x11"):
    raise RuntimeError("Unknown parent mode")
parent_window = None
parent_handle = ""
parent_identity = None
host_display = os.environ.get("DISPLAY")
parent_clicks = 0
parent_sequence = 0
state_path = Path(os.environ.get("ROOST_COMPOSITOR_STATE", "/out/compositor-state.json"))
def scene():
    return json.loads(state_path.read_text())
def pump_parent_until(predicate, message):
    end = time.monotonic() + 5
    while time.monotonic() < end:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        if predicate():
            return
        time.sleep(.01)
    raise RuntimeError(message)
def click_parent_content():
    current = scene()
    parent, = [w for w in current["windows"] if w["id"] == parent_identity["id"]]
    x, y, width, height = parent["rect"]
    if width < 100 or height < 150 or current["focused"] != parent_identity["id"]:
        raise RuntimeError("Actual parent content is not ready for positive input control")
    if not host_display:
        raise RuntimeError("Missing nested host display for real pointer control")
    env = dict(os.environ, DISPLAY=host_display)
    windows = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name", "^Smithay"], env=env, text=True).split()
    if len(windows) != 1:
        raise RuntimeError("Expected one nested host for real parent input control")
    geometry = dict(line.split("=", 1) for line in subprocess.check_output(
        ["xdotool", "getwindowgeometry", "--shell", windows[0]], env=env, text=True).splitlines())
    if not (0 <= x + 50 < int(geometry["WIDTH"]) and 0 <= y + 100 < int(geometry["HEIGHT"])):
        raise RuntimeError("Parent positive input control lies outside the nested host")
    subprocess.run(["xdotool", "mousemove", "--window", windows[0], str(x + 50), str(y + 100), "click", "1"], env=env, check=True)
if parent_mode == "x11":
    display = scene()["x11_display"]
    if not display:
        raise RuntimeError("Roost did not reserve a rootless X11 display")
    os.environ["DISPLAY"] = display
    os.environ["GDK_BACKEND"] = "x11"
    os.environ.setdefault("GSK_RENDERER", "cairo")
    GLib.set_prgname("roost-x11-picker-parent")
    gi.require_version("Gtk", "4.0")
    gi.require_version("GdkX11", "4.0")
    from gi.repository import Gtk, GdkX11
    Gtk.init()
    parent_window = Gtk.Window(title="Roost X11 picker parent")
    parent_window.set_default_size(1100, 700)
    parent_button = Gtk.Button(label="Actual GTK X11 parent input control")
    def clicked(_button):
        global parent_clicks
        parent_clicks += 1
    parent_button.connect("clicked", clicked)
    parent_window.set_child(parent_button)
    input_path = out.with_suffix(".parent-input.json")
    def record_parent_input():
        global parent_sequence
        parent_sequence += 1
        temporary = input_path.with_suffix(".tmp")
        temporary.write_text(json.dumps({"clicks": parent_clicks, "sequence": parent_sequence, "pid": os.getpid()}))
        temporary.replace(input_path)
        return True
    GLib.timeout_add(50, record_parent_input)
    parent_window.present()
    end = time.monotonic() + 10
    while time.monotonic() < end:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        native = parent_window.get_surface()
        if native is not None:
            xid = GdkX11.X11Surface.get_xid(native)
            candidates = [w for w in scene()["windows"] if w.get("x11_window_id") == xid]
            if len(candidates) == 1:
                parent_identity = {"xid": xid, "id": candidates[0]["id"], "pid": os.getpid(), "display": display, "input_path": str(input_path)}
                parent_handle = "x11:" + format(xid, "x")
                break
        time.sleep(.05)
    else:
        raise RuntimeError("Actual GTK parent did not map on Roost's X11 display")
    pump_parent_until(lambda: scene()["focused"] == parent_identity["id"], "Actual X11 caller did not receive initial focus")
    pump_parent_until(lambda: parent_button.get_mapped() and parent_button.get_width() >= 1000
                      and any(w["id"] == parent_identity["id"] and w["rect"][2] >= 1000 for w in scene()["windows"]),
                      "Actual X11 caller content did not allocate and commit its probe size")
    # Real host pointer input must reach the GTK button before requesting a dialog.
    click_parent_content()
    pump_parent_until(lambda: parent_clicks == 1, "Initial real pointer click did not reach the X11 parent button")
    record_parent_input()
    out.with_suffix(".parent-before.json").write_text(input_path.read_text())
fixture = out.parent / "filechooser-fixtures"
fixture.mkdir(exist_ok=True)
payload = b"Roost genuine GNOME FileChooser application data\n"
source = fixture / "open-proof.txt"
source.write_bytes(payload)
expected = source if method == "OpenFile" else fixture / (("saved-proof" if decision == "grant" else "cancel-save-proof") + ("-x11" if parent_mode == "x11" else "") + ".txt")
if method == "SaveFile" and expected.exists():
    raise RuntimeError("Save fixture must start without a destination")
title = "Roost " + method + " " + decision + " proof"
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
def call(dest, path, iface, name, args):
    return bus.call_sync(dest, path, iface, name, args, None, Gio.DBusCallFlags.NONE, 10000, None).unpack()
def nautilus_identity():
    owner, = call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner", GLib.Variant("(s)", ("org.gnome.Nautilus",)))
    pid, = call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetConnectionUnixProcessID", GLib.Variant("(s)", (owner,)))
    exe = Path(os.readlink(f"/proc/{pid}/exe"))
    installed = Path("/usr/bin/nautilus").resolve(strict=True)
    info = installed.stat()
    if exe != installed or info.st_uid != 0 or info.st_mode & 0o022:
        raise RuntimeError("FileChooser is not the genuine installed Nautilus")
    return {"owner": owner, "pid": pid, "executable": str(exe)}
responses = {}
def response(conn, sender, path, iface, signal, params, data):
    responses[path] = params.unpack()
bus.signal_subscribe(None, "org.freedesktop.portal.Request", "Response", None, None,
                     Gio.DBusSignalFlags.NONE, response, None)
options = {"handle_token": GLib.Variant("s", "filechooserproof"),
           "accept_label": GLib.Variant("s", "Choose proof file"),
           "current_folder": GLib.Variant("ay", os.fsencode(fixture) + b"\0")}
if method == "SaveFile":
    options["current_name"] = GLib.Variant("s", expected.name)
handle, = call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
              "org.freedesktop.portal.FileChooser", method,
              GLib.Variant("(ssa{sv})", (parent_handle, title, options)))
# An actual UI decision must precede any successful file grant.
end = time.monotonic() + 1
while time.monotonic() < end:
    GLib.MainContext.default().iteration(False)
    if handle in responses:
        raise RuntimeError("FileChooser responded before actual user interaction: " + str(responses[handle]))
    time.sleep(.01)
end = time.monotonic() + 10
while True:
    try:
        identity = nautilus_identity()
        break
    except GLib.Error as error:
        if "NameHasNoOwner" not in error.message or time.monotonic() >= end:
            raise
        GLib.MainContext.default().iteration(False)
        time.sleep(.05)
out.with_suffix(".waiting.json").write_text(json.dumps({"method": method, "decision": decision, "title": title, "path": str(expected), "provider": identity, "parent": parent_identity}, indent=2))
end = time.monotonic() + 45
while handle not in responses and time.monotonic() < end:
    GLib.MainContext.default().iteration(False)
    time.sleep(.01)
if handle not in responses:
    raise RuntimeError("Nautilus FileChooser response timed out")
code, results = responses[handle]
if nautilus_identity() != identity:
    raise RuntimeError("Nautilus provider owner or executable changed during request")
out.with_suffix(".response.json").write_text(json.dumps({"response": code, "results": results, "provider": identity}, indent=2))
if parent_identity is not None:
    end = time.monotonic() + 5
    while time.monotonic() < end:
        GLib.MainContext.default().iteration(False)
        current = scene()
        if current["focused"] == parent_identity["id"]:
            out.with_suffix(".parent-return.json").write_text(json.dumps(current, indent=2))
            break
        time.sleep(.05)
    else:
        raise RuntimeError("Closing the actual portal dialog did not return focus to its X11 parent")
    if parent_clicks != 1:
        raise RuntimeError("Modal picker leaked pointer activation to its X11 parent")
    click_parent_content()
    pump_parent_until(lambda: parent_clicks == 2, "Parent pointer input did not resume after closing the actual picker")
    record_parent_input()
    out.with_suffix(".parent-after.json").write_text(input_path.read_text())
if decision == "cancel":
    if code != 1 or results.get("uris"):
        raise RuntimeError("Cancel did not reject the file grant")
    if method == "SaveFile" and expected.exists():
        raise RuntimeError("Cancel created a destination")
    print(method + " Cancel: no file grant")
    sys.exit(0)
if code != 0 or len(results.get("uris", [])) != 1:
    raise RuntimeError("FileChooser grant failed: " + str((code, results)))
uri = urlparse(results["uris"][0])
if uri.scheme != "file" or uri.netloc or Path(unquote(uri.path)) != expected:
    raise RuntimeError("FileChooser returned the wrong local file URI")
if method == "SaveFile":
    with expected.open("xb") as file:
        file.write(payload)
if expected.read_bytes() != payload:
    raise RuntimeError("Application Open/Save did not roundtrip actual file bytes")
out.with_suffix(".content.json").write_text(json.dumps({"path": str(expected), "sha256": hashlib.sha256(payload).hexdigest(), "bytes": len(payload)}, indent=2))
print(method + " actual Nautilus selection and application file roundtrip pass")
