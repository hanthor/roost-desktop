#!/usr/bin/python3
"""Exercise the installed GNOME/Nautilus FileChooser, retaining actual responses."""
import atexit
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
if method not in ("OpenFile", "SaveFile") or decision not in ("cancel", "grant", "close"):
    raise RuntimeError("Unknown file-picker operation or decision")
if parent_mode not in ("none", "x11", "wayland"):
    raise RuntimeError("Unknown parent mode")
parent_window = None
parent_handle = ""
parent_identity = None
host_display = os.environ.get("DISPLAY")
parent_clicks = 0
parent_keys = 0
unrelated_process = None
unrelated_before = None
parent_sequence = 0
parent_paints = 0
parent_painted_size = None
parent_pointer_probes = []
original_pointer_host = None
state_path = Path(os.environ.get("ROOST_COMPOSITOR_STATE", "/out/compositor-state.json"))
def scene():
    return json.loads(state_path.read_text())
def pump_parent_until(predicate, message, timeout=5):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        if predicate():
            return
        time.sleep(.01)
    if parent_window is not None:
        out.with_suffix(".parent-failure.json").write_text(json.dumps({
            "failure": message, "parent": parent_identity, "scene": scene(),
            "window_size": [parent_window.get_width(), parent_window.get_height()],
            "button_size": [parent_button.get_width(), parent_button.get_height()],
            "button_mapped": parent_button.get_mapped(),
            "client_after_paint_count": parent_paints,
            "client_last_painted_size": parent_painted_size,
            "pointer_probes": parent_pointer_probes,
        }, indent=2))
    raise RuntimeError(message)
def measured_parent_target():
    valid, bounds = parent_button.compute_bounds(parent_window)
    if not valid or not parent_button.get_mapped() or not parent_button.is_sensitive():
        raise RuntimeError("Original parent button has no mapped sensitive bounds")
    bx, by, bw, bh = bounds.get_x(), bounds.get_y(), bounds.get_width(), bounds.get_height()
    if bw < 100 or bh < 150:
        raise RuntimeError("Original parent button bounds are too small for input proof")
    # Keep the left content target: the separate application B intentionally
    # overlaps the right of A later in this same journey.
    wx, wy = bx + min(50, bw / 2), by + min(100, bh / 2)
    picked = parent_window.pick(wx, wy, Gtk.PickFlags.DEFAULT)
    widget, chain = picked, []
    while widget is not None:
        chain.append(widget.__gtype__.name)
        if widget == parent_button:
            break
        widget = widget.get_parent()
    if widget != parent_button:
        raise RuntimeError("Measured original client point does not hit its actual button")
    # GTK documents this as surface -> native widget translation. Invert it
    # to turn the measured widget point into native surface coordinates.
    tx, ty = parent_window.get_surface_transform()
    sx, sy = wx - tx, wy - ty
    surface = parent_window.get_surface()
    if not (0 <= sx < surface.get_width() and 0 <= sy < surface.get_height()):
        raise RuntimeError("Measured button point lies outside original native surface")
    return {"bounds": [bx, by, bw, bh], "widget_point": [wx, wy],
            "surface_transform": [tx, ty], "surface_point": [sx, sy],
            "surface_size": [surface.get_width(), surface.get_height()],
            "picked_ancestry": chain, "client_after_paint_count": parent_paints,
            "client_last_painted_size": parent_painted_size}

def click_parent_content():
    global original_pointer_host
    current = scene()
    parent, = [w for w in current["windows"] if w["id"] == parent_identity["id"]]
    if current["focused"] != parent_identity["id"] or parent["x11_window_id"] != parent_identity["xid"] or parent["app_id"] != parent_identity["app_id"]:
        raise RuntimeError("Original parent identity/focus changed before positive input")
    if not host_display:
        raise RuntimeError("Missing nested host display for real pointer control")
    env = dict(os.environ, DISPLAY=host_display)
    windows = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name", "^Smithay"], env=env, text=True, timeout=5).split()
    if len(windows) != 1:
        raise RuntimeError("Expected one nested host for real parent input control")
    host = windows[0]
    pid = int(subprocess.check_output(["xdotool", "getwindowpid", host], env=env, text=True, timeout=5))
    process = Path(f"/proc/{pid}")
    executable = (process / "exe").resolve(strict=True)
    candidate = Path("/candidate/usr/bin/roost-compositor").resolve(strict=True)
    if executable != candidate or process.stat().st_uid != os.getuid():
        raise RuntimeError("Pointer host is not the original ordinary-user candidate")
    identity = {"window": int(host), "pid": pid,
                "start_ticks": (process / "stat").read_text().rsplit(")", 1)[1].split()[19],
                "executable": str(executable), "sha256": hashlib.sha256(executable.read_bytes()).hexdigest()}
    if original_pointer_host is None:
        original_pointer_host = identity
    elif original_pointer_host != identity:
        raise RuntimeError("Original owned pointer host identity changed")
    geometry = dict(line.split("=", 1) for line in subprocess.check_output(
        ["xdotool", "getwindowgeometry", "--shell", host], env=env, text=True, timeout=5).splitlines())
    target = measured_parent_target()
    x, y, width, height = parent["rect"]
    px, py = round(x + target["surface_point"][0]), round(y + target["surface_point"][1])
    if not (0 <= px < int(geometry["WIDTH"]) and 0 <= py < int(geometry["HEIGHT"])):
        raise RuntimeError("Measured original parent target lies outside actual nested host")
    # Establish host focus before moving/clicking, never between button events.
    subprocess.run(["xdotool", "windowfocus", "--sync", host], env=env, check=True, timeout=5)
    focus = int(subprocess.check_output(["xdotool", "getwindowfocus"], env=env, text=True, timeout=5))
    if focus != int(host):
        raise RuntimeError("Original pointer host did not acquire actual X focus")
    probe = {"host": identity, "host_geometry": geometry, "actual_host_focus": focus,
             "parent": parent_identity, "parent_rect": parent["rect"], "target": target,
             "host_point": [px, py], "phase": "before_move"}
    if len(parent_pointer_probes) >= 4:
        raise RuntimeError("Parent input proof observation budget exceeded")
    parent_pointer_probes.append(probe)
    out.with_suffix(".parent-pointer.json").write_text(json.dumps(parent_pointer_probes, indent=2))
    subprocess.run(["xdotool", "mousemove", "--window", host, str(px), str(py)], env=env, check=True, timeout=5)
    def at_target():
        observed = scene()
        return (observed["pointer_position"] == [px, py] and observed["focused"] == parent_identity["id"]
                and any(w["id"] == parent_identity["id"] and w["rect"] == parent["rect"] for w in observed["windows"]))
    pump_parent_until(at_target, "Original parent/actual pointer did not settle at measured target")
    pointer = dict(line.split("=", 1) for line in subprocess.check_output(
        ["xdotool", "getmouselocation", "--shell"], env=env, text=True, timeout=5).splitlines())
    if [int(pointer["X"]), int(pointer["Y"])] != [int(geometry["X"]) + px, int(geometry["Y"]) + py]:
        raise RuntimeError("Actual X pointer differs from measured original host point")
    if measured_parent_target()["surface_point"] != target["surface_point"]:
        raise RuntimeError("Original client button moved before the single click")
    if (process / "stat").read_text().rsplit(")", 1)[1].split()[19] != identity["start_ticks"] or int(subprocess.check_output(["xdotool", "getwindowfocus"], env=env, text=True, timeout=5)) != focus:
        raise RuntimeError("Original host identity/focus changed before the single click")
    probe.update(phase="before_single_click", actual_pointer=pointer,
                 actual_scene_pointer=scene()["pointer_position"])
    out.with_suffix(".parent-pointer.json").write_text(json.dumps(parent_pointer_probes, indent=2))
    subprocess.run(["xdotool", "click", "1"], env=env, check=True, timeout=5)
if parent_mode != "none":
    display = scene()["x11_display"] if parent_mode == "x11" else os.environ.get("WAYLAND_DISPLAY")
    if not display:
        raise RuntimeError("Roost did not provide the requested parent display")
    if parent_mode == "x11":
        os.environ["DISPLAY"] = display
    os.environ.pop("WAYLAND_SOCKET", None)
    os.environ["GDK_BACKEND"] = parent_mode
    os.environ.setdefault("GSK_RENDERER", "cairo")
    GLib.set_prgname("roost-" + parent_mode + "-picker-parent")
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gtk, Gdk
    if parent_mode == "x11":
        gi.require_version("GdkX11", "4.0")
        from gi.repository import GdkX11
    else:
        gi.require_version("GdkWayland", "4.0")
        from gi.repository import GdkWayland
    Gtk.init()
    parent_title = "Roost " + parent_mode + " picker parent"
    parent_window = Gtk.Window(title=parent_title)
    parent_window.set_default_size(1100, 700)
    parent_button = Gtk.Button(label="Actual GTK " + parent_mode + " parent input control")
    def clicked(_button):
        global parent_clicks
        parent_clicks += 1
    parent_button.connect("clicked", clicked)
    parent_window.set_child(parent_button)
    key_controller = Gtk.EventControllerKey()
    key_controller.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    def parent_key_pressed(_controller, keyval, _keycode, _state):
        global parent_keys
        if keyval == Gdk.KEY_F8:
            parent_keys += 1
            return True
        return False
    key_controller.connect("key-pressed", parent_key_pressed)
    parent_window.add_controller(key_controller)
    input_path = out.with_suffix(".parent-input.json")
    def record_parent_input():
        global parent_sequence
        parent_sequence += 1
        temporary = input_path.with_suffix(".tmp")
        temporary.write_text(json.dumps({"clicks": parent_clicks, "keys": parent_keys, "sequence": parent_sequence, "pid": os.getpid()}))
        temporary.replace(input_path)
        return True
    GLib.timeout_add(50, record_parent_input)
    parent_window.present()
    def painted(_clock):
        global parent_paints, parent_painted_size
        parent_paints += 1
        parent_painted_size = [parent_window.get_width(), parent_window.get_height()]
    parent_window.get_frame_clock().connect("after-paint", painted)
    parent_app_id = "org.example.RoostWaylandPickerParent"
    parent_app_id_set = False
    end = time.monotonic() + 10
    while time.monotonic() < end:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        native = parent_window.get_surface()
        if native is not None:
            if parent_mode == "wayland" and not parent_app_id_set:
                GdkWayland.WaylandToplevel.set_application_id(native, parent_app_id)
                parent_app_id_set = True
            xid = GdkX11.X11Surface.get_xid(native) if parent_mode == "x11" else None
            candidates = [w for w in scene()["windows"]
                          if (w.get("x11_window_id") == xid if parent_mode == "x11"
                              else w.get("app_id") == parent_app_id and w.get("x11_window_id") is None)]
            if len(candidates) == 1:
                parent_identity = {"mode": parent_mode, "xid": xid, "id": candidates[0]["id"], "app_id": candidates[0]["app_id"], "pid": os.getpid(), "uid": os.getuid(), "start_ticks": Path("/proc/self/stat").read_text().rsplit(")", 1)[1].split()[19], "display": display, "input_path": str(input_path)}
                if parent_mode == "x11":
                    parent_handle = "x11:" + format(xid, "x")
                break
        time.sleep(.05)
    else:
        raise RuntimeError("Actual GTK parent did not map on Roost's requested display")
    if parent_mode == "wayland":
        exported_handles = []
        def exported(_surface, handle, _data):
            exported_handles.append(handle)
        if not GdkWayland.WaylandToplevel.export_handle(native, exported, None):
            raise RuntimeError("GTK refused to export the actual Wayland parent surface")
        pump_parent_until(lambda: bool(exported_handles), "Actual Wayland parent handle was not exported")
        if len(exported_handles) != 1 or not exported_handles[0]:
            raise RuntimeError("Actual Wayland parent export did not identify one handle")
        parent_handle = "wayland:" + exported_handles[0]
        out.with_suffix(".parent-export.json").write_text(json.dumps({
            "parent": parent_identity, "handle": parent_handle, "pid": os.getpid(),
        }, indent=2))
    pump_parent_until(lambda: scene()["focused"] == parent_identity["id"], "Actual caller did not receive initial focus")
    pump_parent_until(lambda: parent_paints > 0 and parent_painted_size == [parent_window.get_width(), parent_window.get_height()] and parent_button.get_mapped() and parent_button.get_width() >= 1000
                      and any(w["id"] == parent_identity["id"] and w["rect"][2] >= 1000 for w in scene()["windows"]),
                      "Actual original client did not map, allocate and complete its initial paint")
    # Real host pointer input must reach the GTK button before requesting a dialog.
    click_parent_content()
    pump_parent_until(lambda: parent_clicks == 1, "Initial real pointer click did not reach the parent button")
    keyboard_probes = []
    def send_focus_key():
        # Compositor focus and Xvfb host keyboard focus are separate. A pointer
        # click proves pointer delivery but does not establish the latter in
        # this headless X server without a window manager. Target the genuine
        # nested host before injecting each single real key, as the GTK proof
        # already does. Do not activate A/B/C or replay failed input.
        env = dict(os.environ, DISPLAY=host_display)
        hosts = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name", "^Smithay"],
                                        env=env, text=True, timeout=5).split()
        if len(hosts) != 1:
            raise RuntimeError("Expected one nested host for actual keyboard input")
        host = hosts[0]
        host_pid = int(subprocess.check_output(["xdotool", "getwindowpid", host], env=env, text=True, timeout=5))
        executable = Path(f"/proc/{host_pid}/exe").resolve(strict=True)
        if executable != Path("/candidate/usr/bin/roost-compositor").resolve(strict=True):
            raise RuntimeError("Keyboard host is not the actual candidate compositor")
        start = Path(f"/proc/{host_pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
        subprocess.run(["xdotool", "windowfocus", "--sync", host], env=env, check=True, timeout=5)
        actual_focus = int(subprocess.check_output(["xdotool", "getwindowfocus"], env=env, text=True, timeout=5))
        if actual_focus != int(host):
            raise RuntimeError("Nested X host did not acquire actual keyboard focus")
        keyboard_probes.append({"host_window": int(host), "host_pid": host_pid,
            "host_executable": str(executable), "host_start_time": start,
            "actual_x_focus": actual_focus, "scene_before_key": scene()})
        out.with_suffix(".keyboard-host.json").write_text(json.dumps(keyboard_probes, indent=2))
        subprocess.run(["xdotool", "key", "F8"], env=env, check=True)
    send_focus_key()
    pump_parent_until(lambda: parent_keys == 1, "Initial actual keyboard input did not reach caller")
    record_parent_input()
    out.with_suffix(".parent-before.json").write_text(input_path.read_text())
    # B is a separate ordinary Wayland process, mapped and focused after caller
    # A. The original exported handle must still parent Nautilus C to A.
    unrelated_path = out.with_suffix(".unrelated-input.json")
    unrelated_env = dict(os.environ, DISPLAY=host_display, GDK_BACKEND="wayland")
    unrelated_log = out.with_suffix(".unrelated.log").open("w")
    unrelated_process = subprocess.Popen([sys.executable,
        str(Path(__file__).with_name("roost-picker-unrelated-app.py")), str(unrelated_path)],
        env=unrelated_env, stdout=unrelated_log, stderr=subprocess.STDOUT)
    unrelated_log.close()
    def stop_unrelated():
        if unrelated_process.poll() is None:
            unrelated_process.terminate()
            try:
                unrelated_process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                unrelated_process.kill()
                unrelated_process.wait(timeout=5)
    atexit.register(stop_unrelated)
    def unrelated_state():
        if unrelated_process.poll() is not None:
            raise RuntimeError("Independent application exited during picker journey")
        record = json.loads(unrelated_path.read_text())
        start = Path(f"/proc/{unrelated_process.pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
        if (record["pid"] != unrelated_process.pid or record["uid"] != os.getuid()
                or record["start_time"] != start):
            raise RuntimeError("Independent application identity changed")
        return record
    def unrelated_ready():
        if not unrelated_path.exists():
            return False
        record = unrelated_state()
        windows = [w for w in scene()["windows"] if w.get("app_id") == record["app_id"]]
        return (record["mapped"] and len(windows) == 1
                and windows[0]["id"] != parent_identity["id"]
                and scene()["focused"] == windows[0]["id"])
    pump_parent_until(unrelated_ready, "Independent application did not map and receive focus", timeout=10)
    unrelated_window, = [w for w in scene()["windows"]
                        if w.get("app_id") == unrelated_state()["app_id"]]
    ux, uy, uw, uh = unrelated_window["rect"]
    if uw < 100 or uh < 150:
        raise RuntimeError("Independent application content has no usable input target")
    host_windows = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name", "^Smithay"],
        env=unrelated_env, text=True).split()
    if len(host_windows) != 1:
        raise RuntimeError("Expected one nested host for independent input control")
    geometry = dict(line.split("=", 1) for line in subprocess.check_output(
        ["xdotool", "getwindowgeometry", "--shell", host_windows[0]], env=unrelated_env, text=True).splitlines())
    if not (0 <= ux + 50 < int(geometry["WIDTH"]) and 0 <= uy + 100 < int(geometry["HEIGHT"])):
        raise RuntimeError("Independent input control lies outside nested host")
    subprocess.run(["xdotool", "mousemove", "--window", host_windows[0], str(ux + 50), str(uy + 100),
        "click", "1"], env=unrelated_env, check=True)
    send_focus_key()
    pump_parent_until(lambda: unrelated_state()["clicks"] == 1 and unrelated_state()["keys"] == 1,
                      "Independent application did not receive real pointer and keyboard input")
    unrelated_before = unrelated_state()
    if parent_clicks != 1 or parent_keys != 1 or scene()["focused"] != unrelated_window["id"]:
        raise RuntimeError("Independent input leaked to caller or failed to leave B focused")
    out.with_suffix(".multiple-apps-before.json").write_text(json.dumps({
        "parent": parent_identity, "unrelated": unrelated_before,
        "unrelated_window_id": unrelated_window["id"], "scene": scene()}, indent=2))
fixture = out.parent / "filechooser-fixtures"
fixture.mkdir(exist_ok=True)
payload = b"Roost genuine GNOME FileChooser application data\n"
source = fixture / "open-proof.txt"
source.write_bytes(payload)
expected = source if method == "OpenFile" else fixture / (("saved-proof" if decision == "grant" else decision + "-save-proof") + ("-" + parent_mode if parent_mode != "none" else "") + ".txt")
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
if decision == "close":
    ready_path = out.with_suffix(".close-ready.json")
    pump_parent_until(ready_path.exists, "Actual file-picker UI was not ready for Request.Close", timeout=30)
    dialog_identity = json.loads(ready_path.read_text())
    if dialog_identity["provider"] != identity:
        raise RuntimeError("Request.Close UI identified a different provider")
    dialog_id = dialog_identity["dialog_id"]
    if handle in responses:
        raise RuntimeError("FileChooser responded before the caller aborted its request")
    before_close = scene()
    if before_close["focused"] != dialog_id or not any(
            window["id"] == dialog_id and window.get("app_id") == "org.gnome.Nautilus"
            for window in before_close["windows"]):
        raise RuntimeError("Original actual file-picker dialog disappeared before Request.Close")
    call("org.freedesktop.portal.Desktop", handle, "org.freedesktop.portal.Request", "Close", None)
    pump_parent_until(lambda: not any(window["id"] == dialog_id for window in scene()["windows"]),
                      "Request.Close did not remove its actual Nautilus dialog")
    # The public Request.Close contract emits no Response. Observe fresh GLib
    # iterations after disappearance instead of treating a returned method as
    # proof of teardown, or synthesizing a cancellation response.
    close_observations = []
    def observe_close():
        current = scene()
        if handle in responses or any(window["id"] == dialog_id for window in current["windows"]):
            close_observations.append({"failure": "response or original dialog remained", "scene": current})
            return False
        close_observations.append({"sequence": len(close_observations) + 1, "scene": current})
        return len(close_observations) < 10
    GLib.timeout_add(50, observe_close)
    pump_parent_until(lambda: len(close_observations) >= 10 or any(
        "failure" in row for row in close_observations), "No fresh post-close observations")
    out.with_suffix(".request-close.json").write_text(json.dumps({
        "handle": handle, "provider": identity, "dialog_id": dialog_id,
        "before": before_close, "observations": close_observations,
        "response_emitted": handle in responses,
        "save_destination_exists": expected.exists() if method == "SaveFile" else None,
    }, indent=2))
    if any("failure" in row for row in close_observations) or handle in responses:
        raise RuntimeError("Aborted portal request emitted a response or retained its dialog")
    if nautilus_identity() != identity:
        raise RuntimeError("Request.Close replaced the original Nautilus provider")
    if method == "SaveFile" and expected.exists():
        raise RuntimeError("Request.Close created a save destination")
else:
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
        raise RuntimeError("Closing the actual portal dialog did not return focus to its parent")
    if parent_clicks != 1:
        raise RuntimeError("Modal picker leaked pointer activation to its parent")
    click_parent_content()
    pump_parent_until(lambda: parent_clicks == 2, "Parent pointer input did not resume after closing the actual picker")
    unrelated_return_sequence = unrelated_state()["sequence"]
    send_focus_key()
    pump_parent_until(lambda: parent_keys == 2, "Keyboard input did not return to actual caller A")
    pump_parent_until(lambda: unrelated_state()["sequence"] >= unrelated_return_sequence + 10,
                      "Independent client stopped processing events during picker journey")
    unrelated_after = unrelated_state()
    if (unrelated_after["clicks"] != 1 or unrelated_after["keys"] != 1
            or parent_clicks != 2 or parent_keys != 2):
        raise RuntimeError("Picker teardown leaked input or restored unrelated application B")
    out.with_suffix(".multiple-apps-after.json").write_text(json.dumps({
        "parent": parent_identity, "parent_clicks": parent_clicks, "parent_keys": parent_keys,
        "unrelated_before": unrelated_before, "unrelated_after": unrelated_after,
        "scene": scene()}, indent=2))
    record_parent_input()
    out.with_suffix(".parent-after.json").write_text(input_path.read_text())
    print("G-FILECHOOSER-MULTIPLE-APPS: original caller pointer/keyboard restored; independent client unchanged")
    if parent_mode == "wayland":
        GdkWayland.WaylandToplevel.drop_exported_handle(native, exported_handles[0])
if decision == "close":
    while GLib.MainContext.default().pending():
        GLib.MainContext.default().iteration(False)
    if handle in responses:
        raise RuntimeError("Aborted portal request emitted a response during parent restoration")
    print(method + " Request.Close: actual dialog removed, no response or file grant")
    sys.exit(0)
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
