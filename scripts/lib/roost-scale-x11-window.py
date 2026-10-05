#!/usr/bin/python3
"""GTK3/X11 painted button; only a delivered click increments the fixture."""
import os
from pathlib import Path
import sys

os.environ['GDK_BACKEND'] = 'x11'
import gi  # noqa: E402
gi.require_version('Gtk', '3.0')
from gi.repository import GLib, Gtk  # noqa: E402

GLib.set_prgname('roost-scale-x11')
output = Path(sys.argv[1])
count = 0
loop = GLib.MainLoop()
window = Gtk.Window(title='Roost Scale X11')
window.set_wmclass('roost-scale-x11', 'roost-scale-x11')
window.set_default_size(240, 160)
window.connect('destroy', lambda *_: loop.quit())
button = Gtk.Button(label='Clicks: 0')
css = Gtk.CssProvider()
css.load_from_data(b'button { background-image: none; background-color: #c061cb; color: white; }')
Gtk.StyleContext.add_provider_for_screen(window.get_screen(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)


def clicked(_button):
    global count
    count += 1
    button.set_label(f'Clicks: {count}')
    temporary = output.with_suffix('.tmp')
    temporary.write_text(str(count))
    os.replace(temporary, output)


output.write_text('0')
button.connect('clicked', clicked)
window.add(button)
window.show_all()
loop.run()
