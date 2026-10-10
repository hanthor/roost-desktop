#!/usr/bin/python3
"""A real Wayland GTK client records the key and pointer events the
compositor delivers, for the keyboard accessibility aid proofs (#350).

Presses carry the keyval and the Shift/Control state GTK saw with them,
so a latched modifier shows as an ordinary chord."""
import json
import sys
from pathlib import Path
import gi
gi.require_version('Gtk', '4.0')
gi.require_version('Gdk', '4.0')
from gi.repository import Gdk, Gtk

state = {'presses': [], 'releases': [], 'buttons': []}
output = Path(sys.argv[1])


def save():
    temporary = output.with_suffix('.tmp')
    temporary.write_text(json.dumps(state))
    temporary.replace(output)


def pressed(_controller, keyval, keycode, mods):
    state['presses'].append({
        'keyval': keyval,
        'keycode': keycode,
        'shift': bool(mods & Gdk.ModifierType.SHIFT_MASK),
        'ctrl': bool(mods & Gdk.ModifierType.CONTROL_MASK),
    })
    save()
    # Keep keys out of the entry: the recording is the whole point.
    return True


def released(_controller, keyval, _keycode, _mods):
    state['releases'].append(keyval)
    save()


def button(gesture, _count, x, y):
    state['buttons'].append({'button': gesture.get_current_button(), 'x': x, 'y': y})
    save()


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title='A11y Input Proof')
    window.set_default_size(900, 600)
    window.set_child(Gtk.Label(label='Keyboard accessibility proof'))
    keys = Gtk.EventControllerKey()
    keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    keys.connect('key-pressed', pressed)
    keys.connect('key-released', released)
    window.add_controller(keys)
    click = Gtk.GestureClick()
    click.set_button(0)
    click.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    click.connect('pressed', button)
    window.add_controller(click)
    window.present()
    save()


app = Gtk.Application(application_id='org.tuna.A11yInputProof')
app.connect('activate', activate)
app.run([])
