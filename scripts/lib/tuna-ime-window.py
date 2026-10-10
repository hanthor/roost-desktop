#!/usr/bin/python3
"""A GTK window with one focused text field, for the IME proof.

Usage: tuna-ime-window.py OUT

Writes the field's text to OUT whenever it changes. GTK talks to the
compositor's text-input-v3 (GTK_IM_MODULE=wayland), as in a GNOME
session.
"""
import os
import sys

os.environ.setdefault("GSK_RENDERER", "cairo")
os.environ.setdefault("GTK_IM_MODULE", "wayland")

import gi  # noqa: E402

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

OUT = sys.argv[1]


def changed(entry):
    with open(OUT + ".tmp", "w", encoding="utf-8") as fh:
        fh.write(entry.get_text())
    os.replace(OUT + ".tmp", OUT)


GLib.set_prgname("tuna-ime")
loop = GLib.MainLoop()
win = Gtk.Window(title="IME")
win.set_default_size(480, 160)
win.connect("close-request", lambda *_: loop.quit() or False)
entry = Gtk.Entry()
entry.update_property([Gtk.AccessibleProperty.LABEL], ["IME Field"])
entry.connect("changed", changed)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
box.append(entry)
menu = Gtk.MenuButton(label="IME Popover")
popover = Gtk.Popover()
popup_entry = Gtk.Entry()
popup_entry.update_property([Gtk.AccessibleProperty.LABEL], ["IME Popover Field"])
popover.set_child(popup_entry)
popover.connect("show", lambda *_: popup_entry.grab_focus())
# No grab on "closed": grabbing while the popover hides races the
# compositor's focus restore and leaves in-client focus anywhere but the
# entry (#566). The proof clicks the entry itself when it needs focus.
menu.set_popover(popover)
box.append(menu)
win.set_child(box)
win.present()
entry.grab_focus()
loop.run()
