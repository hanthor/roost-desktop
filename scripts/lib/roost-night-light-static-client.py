#!/usr/bin/python3
"""Genuine GTK4 layer-shell source pixels; no policy or capture API provider."""
from ctypes import CDLL
CDLL('/usr/lib64/libgtk4-layer-shell.so.0')
import gi
gi.require_version('Gtk','4.0')
gi.require_version('Gdk','4.0')
gi.require_version('Gtk4LayerShell','1.0')
from gi.repository import Gtk, Gdk
from gi.repository import Gtk4LayerShell as LayerShell

COLORS=[(180,120,100),(100,180,120),(120,100,180),(160,160,100),(100,160,160)]


def activate(app):
    window=Gtk.Window(application=app)
    window.set_title('Roost static Night Light pixel source')
    window.set_decorated(False)
    window.set_default_size(120,800)
    LayerShell.init_for_window(window)
    LayerShell.set_namespace(window,'roost-night-light-static-probes')
    LayerShell.set_layer(window,LayerShell.Layer.BOTTOM)
    LayerShell.set_keyboard_mode(window,LayerShell.KeyboardMode.NONE)
    LayerShell.set_exclusive_zone(window,-1)
    for edge in (LayerShell.Edge.LEFT,LayerShell.Edge.TOP,LayerShell.Edge.BOTTOM):
        LayerShell.set_anchor(window,edge,True)
    box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=0)
    css=Gtk.CssProvider()
    css.load_from_data('\n'.join(f'.night-light-probe-{i} {{ background-color: rgb({r},{g},{b}); }}'
                                 for i,(r,g,b) in enumerate(COLORS)).encode())
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(),css,Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
    for i in range(5):
        strip=Gtk.Box()
        strip.set_size_request(120,160)
        strip.add_css_class('night-light-probe-'+str(i))
        box.append(strip)
    window.set_child(box)
    window.present()


app=Gtk.Application(application_id='org.roost.NightLight.StaticProbes')
app.connect('activate',activate)
app.run(None)
