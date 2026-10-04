#!/usr/bin/python3
"""Exercise the actual installed desktop portal frontend and consent backend."""
import json
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import time
from gi.repository import Gio, GLib
dump_spec = importlib.util.spec_from_file_location("roost_pipewire_dump", Path(__file__).with_name("roost-pipewire-dump.py"))
dump_module = importlib.util.module_from_spec(dump_spec)
dump_spec.loader.exec_module(dump_module)
out = Path(sys.argv[1])
mode = sys.argv[2]
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
responses = {}
def response(conn, sender, path, iface, signal, params, data):
    responses[path] = params.unpack()
bus.signal_subscribe(None, "org.freedesktop.portal.Request", "Response", None, None, Gio.DBusSignalFlags.NONE, response, None)
def call(iface, method, args, path="/org/freedesktop/portal/desktop"):
    return bus.call_sync("org.freedesktop.portal.Desktop", path, iface, method, args, None, Gio.DBusCallFlags.NONE, 30000, None).unpack()
def request(method, signature, args):
    handle, = call("org.freedesktop.portal.ScreenCast", method, GLib.Variant(signature, args))
    if method == "Start": out.with_suffix(".ready").write_text(mode)
    deadline = time.monotonic() + 60
    while handle not in responses and time.monotonic() < deadline:
        GLib.MainContext.default().iteration(False)
        time.sleep(.01)
    if handle not in responses: raise RuntimeError(method + " timed out")
    return responses.pop(handle)
def options(token):
    return {"handle_token": GLib.Variant("s", token)}
opts = options("create")
opts["session_handle_token"] = GLib.Variant("s", "roostproof" + str(os.getpid()))
code, result = request("CreateSession", "(a{sv})", (opts,))
if code: raise RuntimeError("CreateSession failed " + str(code))
session = result["session_handle"]
opts = options("select")
opts.update({"types": GLib.Variant("u", 1), "multiple": GLib.Variant("b", False), "cursor_mode": GLib.Variant("u", 1)})
code, result = request("SelectSources", "(oa{sv})", (session, opts))
if code: raise RuntimeError("SelectSources failed " + str(code))
code, result = request("Start", "(osa{sv})", (session, "", options("start")))
out.with_suffix(".response.json").write_text(json.dumps({"mode": mode, "response": code, "results": result}, indent=2))
if mode in ("cancel", "locked"):
    if code == 0: raise RuntimeError(mode + " unexpectedly granted capture")
    print(mode + " denied with response " + str(code))
    sys.exit(0)
if code != 0: raise RuntimeError("Start failed " + str(code))
node = result["streams"][0][0]
reply, fds = bus.call_with_unix_fd_list_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.ScreenCast", "OpenPipeWireRemote", GLib.Variant("(oa{sv})", (session, {})), None, Gio.DBusCallFlags.NONE, 30000, None, None)
fd = fds.get(reply.unpack()[0])
try:
    subprocess.run(["gst-launch-1.0", "-q", "pipewiresrc", "fd=" + str(fd), "path=" + str(node), "num-buffers=1", "!", "videoconvert", "!", "pngenc", "!", "filesink", "location=" + str(out)], pass_fds=(fd,), check=True, timeout=30)
finally:
    os.close(fd)
# An unrelated client cannot close the granted frontend session.
other = Gio.DBusConnection.new_for_address_sync(os.environ["DBUS_SESSION_BUS_ADDRESS"], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
try:
    other.call_sync("org.freedesktop.portal.Desktop", session, "org.freedesktop.portal.Session", "Close", None, None, Gio.DBusCallFlags.NONE, 10000, None)
except GLib.Error:
    pass
else:
    raise RuntimeError("another unique caller closed the granted session")
if mode in ("revoke", "backend-disconnect"):
    out.with_suffix(".active").write_text(str(node))
    deadline = time.monotonic() + 30
    while not out.with_suffix(".revoke").exists() and time.monotonic() < deadline:
        GLib.MainContext.default().iteration(False)
        time.sleep(.01)
    if not out.with_suffix(".revoke").exists():
        raise RuntimeError("runner did not revoke the active grant")
elif mode == "disconnect":
    bus.close_sync(None)
else:
    call("org.freedesktop.portal.Session", "Close", None, session)
revoked_at = time.monotonic()
deadline = time.monotonic() + 2
while time.monotonic() < deadline:
    raw_dump = subprocess.check_output(["pw-dump", "--no-colors"], timeout=1)
    out.with_suffix(".pw-dump.txt").write_bytes(raw_dump)
    nodes = dump_module.dump_objects(raw_dump)
    if not any(item.get("id") == node and item.get("type") == "PipeWire:Interface:Node" for item in nodes):
        out.with_suffix(".revoked.json").write_text(json.dumps(nodes, indent=2))
        print("grant consumed frame from node " + str(node) + "; " + ("lock" if mode == "revoke" else "backend disconnect" if mode == "backend-disconnect" else "client disconnect" if mode == "disconnect" else "Close") + " removed PipeWire node in " + str(round(time.monotonic() - revoked_at, 3)) + "s")
        break
    time.sleep(.05)
else: raise RuntimeError("Close retained PipeWire capture node")
