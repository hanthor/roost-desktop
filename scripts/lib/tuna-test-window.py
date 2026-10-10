#!/usr/bin/python3
"""A plain libadwaita window for proof harnesses.

Usage: tuna-test-window.py TITLE [COLOR]

Maps one Adw.ApplicationWindow titled TITLE with a header bar, a big
label, a menu button whose popover proves xdg popups, and a scrollable
list that proves wheel scrolling. Exits when the window closes. With
TUNA_TEST_POPOVER=1 the popover opens by itself once the window shows
(parity captures cannot rely on headless input). TUNA_TEST_REQUESTS
names a JSON file of window requests; besides the window states,
"dialog" opens a modal dialog over the window, "undialog" destroys it
and "close" destroys the window (cleanly, before exiting).
"""
import os
import sys
import json
from pathlib import Path

# Harness windows must not wait on portals (libadwaita reads the color
# scheme from the settings portal, which can stall without a desktop
# backend) or need GPU rendering.
os.environ.setdefault("ADW_DISABLE_PORTAL", "1")
os.environ.setdefault("GSK_RENDERER", "cairo")

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, GLib, Gtk  # noqa: E402

TITLE = sys.argv[1] if len(sys.argv) > 1 else "Tuna Desktop Test"
COLOR = sys.argv[2] if len(sys.argv) > 2 else "#3584e4"


def build(loop):
    # No Gtk.Application: harness windows must not need a session bus.
    win = Adw.Window(title=TITLE)
    win.connect("close-request", lambda *_: loop.quit() or False)
    win.set_default_size(640, 420)
    css = Gtk.CssProvider()
    css.load_from_string(f".tuna-test {{ background: {COLOR}; color: white; }}")
    Gtk.StyleContext.add_provider_for_display(
        win.get_display(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
    header = Adw.HeaderBar()
    menu = Gtk.MenuButton(icon_name="open-menu-symbolic")
    menu.update_property([Gtk.AccessibleProperty.LABEL], ["Main Menu"])
    pop = Gtk.Popover()
    pop.set_child(Gtk.Label(label="Popover content"))
    menu.set_popover(pop)
    header.pack_end(menu)
    box.append(header)
    label = Gtk.Label(label=TITLE)
    label.add_css_class("title-1")
    label.add_css_class("tuna-test")
    label.set_vexpand(True)
    box.append(label)
    rows = Gtk.ListBox()
    for i in range(60):
        rows.append(Gtk.Label(label=f"Row {i}"))
    scroller = Gtk.ScrolledWindow(min_content_height=150)
    scroller.set_child(rows)
    box.append(scroller)
    win.set_content(box)
    win.present()
    if request_path := os.environ.get("TUNA_TEST_REQUESTS"):
        handled = [-1]
        dialog = [None]

        def open_dialog():
            dialog[0] = Gtk.Window(title=TITLE + " Dialog", transient_for=win, modal=True)
            dialog[0].set_default_size(320, 200)
            dialog[0].set_child(Gtk.Label(label="Dialog"))
            dialog[0].present()

        def close_dialog():
            if dialog[0] is not None:
                dialog[0].destroy()
                dialog[0] = None

        def close_window():
            win.destroy()
            # Let the destroy requests reach the compositor first.
            GLib.timeout_add(500, lambda: loop.quit() or False)

        def request():
            try:
                data = json.loads(Path(request_path).read_text())
                generation = data["generation"]
                if generation != handled[0]:
                    actions = {"fullscreen": win.fullscreen, "unfullscreen": win.unfullscreen,
                               "maximize": win.maximize, "unmaximize": win.unmaximize,
                               "dialog": open_dialog, "undialog": close_dialog,
                               "close": close_window}
                    actions[data["request"]]()
                    handled[0] = generation
                    Path(request_path + ".ack").write_text(str(generation))
            except (OSError, ValueError, KeyError):
                pass
            return True
        GLib.timeout_add(50, request)
    if os.environ.get("TUNA_TEST_POPOVER") == "1":
        GLib.timeout_add(1500, lambda: menu.popup() or False)


# Distinct app ids let journeys target each window (xdg_toplevel app_id).
GLib.set_prgname("tuna-test-" + TITLE.lower())
Adw.init()
main_loop = GLib.MainLoop()
build(main_loop)
main_loop.run()
