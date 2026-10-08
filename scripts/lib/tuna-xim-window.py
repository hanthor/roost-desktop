#!/usr/bin/python3
"""A genuine GTK3 XIM client on the compositor's private XWayland.

GTK4 has no xim input module. GTK3's XIM context talks to ibus-x11;
requiring this module distinguishes XIM composition from native IBus.
"""
import os
import sys

os.environ.setdefault("GDK_BACKEND", "x11")
os.environ.setdefault("GTK_IM_MODULE", "xim")

import gi  # noqa: E402

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402

OUT = sys.argv[1]


def changed(entry):
    with open(OUT + ".tmp", "w", encoding="utf-8") as stream:
        stream.write(entry.get_text())
    os.replace(OUT + ".tmp", OUT)


GLib.set_prgname("tuna-xim")
loop = GLib.MainLoop()
window = Gtk.Window(title="Tuna Desktop XIM")
window.set_wmclass("tuna-xim", "tuna-xim")
window.set_default_size(480, 160)
window.connect("destroy", lambda *_: loop.quit())
entry = Gtk.Entry()
entry.get_accessible().set_name("XIM Field")
entry.connect("changed", changed)
window.add(entry)
window.show_all()
entry.grab_focus()
loop.run()
