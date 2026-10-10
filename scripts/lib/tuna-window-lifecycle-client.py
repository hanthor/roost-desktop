#!/usr/bin/python3
"""Commanded native normal/dialog client for the map/destroy proof."""
import json
import os
from pathlib import Path
os.environ.setdefault("GSK_RENDERER", "cairo")
os.environ.setdefault("ADW_DISABLE_PORTAL", "1")
import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, GLib
GLib.set_prgname("tuna-window-lifecycle-proof")
parent = Gtk.Window(title="Lifecycle normal")
parent.set_default_size(500, 350)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
for color in ("lifecycle-red","lifecycle-blue"):
    half = Gtk.Box(vexpand=True)
    half.add_css_class(color)
    box.append(half)
box.add_css_class("lifecycle-proof")
parent.set_child(box)
css = Gtk.CssProvider()
css.load_from_string(".lifecycle-proof, .lifecycle-red { background: #ef2929; } .lifecycle-blue { background: #3465a4; }")
Gtk.StyleContext.add_provider_for_display(parent.get_display(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
parent.present()
loop = GLib.MainLoop()
command = Path(os.environ["TUNA_LIFECYCLE_COMMAND"])
dialog = None
last = None
def poll():
    global dialog, last
    try:
        data = json.loads(command.read_text())
        if data["generation"] == last:
            return True
        last = data["generation"]
        if data["action"] == "dialog":
            dialog = Gtk.Window(title="Lifecycle dialog", transient_for=parent, modal=True)
            dialog.set_default_size(300, 180)
            content = Gtk.Box()
            content.add_css_class("lifecycle-proof")
            dialog.set_child(content)
            dialog.present()
        elif data["action"] == "close-dialog":
            dialog.destroy()
            dialog = None
        elif data["action"] == "close":
            parent.destroy()
            loop.quit()
    except (OSError, ValueError, KeyError):
        pass
    return True
GLib.timeout_add(20, poll)
loop.run()
