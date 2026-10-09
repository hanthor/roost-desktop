#!/usr/bin/python3
"""A real Wayland GTK client records delivered keys and pointer presses."""
import json
import sys
from pathlib import Path
import gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk, GLib
state = {'keys': [], 'buttons': []}
output = Path(sys.argv[1])
def save():
    temporary = output.with_suffix(".tmp")
    temporary.write_text(json.dumps(state))
    temporary.replace(output)
def key(_controller, keyval, keycode, _mods):
    state['keys'].append({'keyval': keyval, 'keycode': keycode})
    save()
    return False
def button(gesture, _count, x, y):
    state['buttons'].append({'button': gesture.get_current_button(), 'x': x, 'y': y})
    save()
def activate(app):
    window = Gtk.ApplicationWindow(application=app, title='Remote Input Proof')
    window.set_default_size(1000, 650)
    entry = Gtk.Entry()
    entry.set_hexpand(True)
    entry.set_vexpand(True)
    window.set_child(entry)
    keys = Gtk.EventControllerKey()
    keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    keys.connect('key-pressed', key)
    window.add_controller(keys)
    click = Gtk.GestureClick()
    click.set_button(0)
    click.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    click.connect('pressed', button)
    window.add_controller(click)
    window.present()
    entry.grab_focus()
    save()
app = Gtk.Application(application_id='org.tuna.RemoteInputProof')
app.connect('activate', activate)
app.run([])
