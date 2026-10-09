#!/usr/bin/python3
"""Independent ordinary GTK client used by the genuine picker focus journey."""
import json
import os
from pathlib import Path
import sys
import gi

gi.require_version('Gtk', '4.0')
gi.require_version('GdkWayland', '4.0')
from gi.repository import Gtk, Gdk, GdkWayland, GLib

output = Path(sys.argv[1])
os.environ['GDK_BACKEND'] = 'wayland'
os.environ.pop('WAYLAND_SOCKET', None)
GLib.set_prgname('tuna-picker-unrelated-app')
Gtk.init()
window = Gtk.Window(title='Unrelated picker focus control')
window.set_default_size(400, 300)
button = Gtk.Button(label='Independent application input control')
window.set_child(button)
clicks = keys = sequence = 0
app_id = 'org.example.TunaPickerUnrelated'
start_time = Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split()[19]

def clicked(_button):
    global clicks
    clicks += 1
button.connect('clicked', clicked)
controller = Gtk.EventControllerKey()
controller.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
def pressed(_controller, keyval, _keycode, _state):
    global keys
    if keyval == Gdk.KEY_F8:
        keys += 1
        return True
    return False
controller.connect('key-pressed', pressed)
window.add_controller(controller)
identified = False
def record():
    global identified, sequence
    surface = window.get_surface()
    if surface is not None and not identified:
        GdkWayland.WaylandToplevel.set_application_id(surface, app_id)
        identified = True
    sequence += 1
    temporary = output.with_suffix('.tmp')
    temporary.write_text(json.dumps({'pid': os.getpid(), 'uid': os.getuid(),
        'start_time': start_time, 'app_id': app_id, 'clicks': clicks,
        'keys': keys, 'sequence': sequence, 'mapped': button.get_mapped()}))
    temporary.replace(output)
    return True
GLib.timeout_add(50, record)
window.present()
GLib.MainLoop().run()
