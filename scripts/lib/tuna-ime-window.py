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


def refocus_field(*_):
    """Return the keyboard to the main field once the popover closes.

    GTK 4.16 and later focus the popover's parent (the menu button) after
    "closed" is emitted, so a grab inside the handler would be undone.
    """

    def grab():
        entry.grab_focus()
        return GLib.SOURCE_REMOVE

    GLib.idle_add(grab)


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
popover.connect("closed", refocus_field)
menu.set_popover(popover)
box.append(menu)
win.set_child(box)
win.present()
entry.grab_focus()
loop.run()
