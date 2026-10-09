#!/usr/bin/python3
"""Use the installed Screenshot portal and retain its actual response/image."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import time
from urllib.parse import unquote, urlparse
import gi
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Gio, GLib, GdkPixbuf

out = Path(sys.argv[1])
mode = sys.argv[2]
def pictures():
    root = Path(os.environ["XDG_PICTURES_DIR"]) / "Screenshots"
    return {str(path): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in root.glob("*.png")}
before_locked = pictures() if mode == "locked" else None
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
responses = {}
def response(conn, sender, path, iface, signal, params, data):
    responses[path] = params.unpack()
bus.signal_subscribe(None, "org.freedesktop.portal.Request", "Response", None, None,
                     Gio.DBusSignalFlags.NONE, response, None)
handle, = bus.call_sync(
    "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
    "org.freedesktop.portal.Screenshot", "Screenshot",
    GLib.Variant("(sa{sv})", ("", {"handle_token": GLib.Variant("s", "shotproof"),
                                  "interactive": GLib.Variant("b", False)})),
    None, Gio.DBusCallFlags.NONE, 30000, None).unpack()
# The application must receive no screenshot before the actual picker decision.
if mode != "locked":
    until = time.monotonic() + 1
    while time.monotonic() < until:
        GLib.MainContext.default().iteration(False)
        if handle in responses:
            raise RuntimeError("Screenshot returned before user consent: " + str(responses[handle]))
        time.sleep(.01)
out.with_suffix(".waiting").write_text(mode)
deadline = time.monotonic() + (10 if mode == "locked" else 60)
while handle not in responses and time.monotonic() < deadline:
    GLib.MainContext.default().iteration(False)
    time.sleep(.01)
if handle not in responses:
    raise RuntimeError("Screenshot timed out")
code, results = responses.pop(handle)
out.with_suffix(".response.json").write_text(json.dumps({"response": code, "results": results}, indent=2))
if mode == "cancel":
    if code == 0 or results.get("uri"):
        raise RuntimeError("Deny exposed a screenshot")
    print("Deny: no screenshot URI, response " + str(code))
    sys.exit(0)
if mode == "locked":
    # GNOME51's backend translates Shell.Screenshot(false, "") to response0
    # with an empty URI. It completes the request without delivering an image.
    if code != 0 or results.get("uri") != "" or out.exists():
        raise RuntimeError("locked Screenshot did not complete with an empty URI")
    after_locked = pictures()
    if after_locked != before_locked:
        raise RuntimeError("locked Screenshot created or changed an image")
    out.with_suffix(".unchanged-images.json").write_text(json.dumps(after_locked, indent=2))
    print("locked: documented false screenshot completed with no URI/image")
    sys.exit(0)
if code != 0:
    raise RuntimeError("Screenshot grant failed: " + str(code))
uri = urlparse(results.get("uri", ""))
if uri.scheme != "file" or uri.netloc:
    raise RuntimeError("Screenshot did not return a local file URI")
source = Path(unquote(uri.path))
if not 0 < source.stat().st_size <= 32 * 1024 * 1024:
    raise RuntimeError("Screenshot file has an invalid size")
pixbuf = GdkPixbuf.Pixbuf.new_from_file(str(source))
if (pixbuf.get_width(), pixbuf.get_height()) != (1280, 800):
    raise RuntimeError("Screenshot does not match the actual output geometry")
if len(set(bytes(pixbuf.get_pixels()))) < 16:
    raise RuntimeError("Screenshot contains no varied desktop pixels")
shutil.copyfile(source, out)
print("actual Screenshot consent delivered " + str(pixbuf.get_width()) + "x" + str(pixbuf.get_height()) + " pixels")
