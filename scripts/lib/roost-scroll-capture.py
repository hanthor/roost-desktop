#!/usr/bin/python3
"""Capture identical nested GTK strip states and time samples on both compositors.

Usage: roost-scroll-capture.py niri|roost OUT_DIR
Roost requires ROOST_COMPOSITOR_STATE; niri requires NIRI_SOCKET.
"""
import ctypes
import json
import os
from pathlib import Path
import socket
import sys
import time
from PIL import ImageGrab

backend, output = sys.argv[1:]
out = Path(output)
xlib = ctypes.CDLL('libX11.so.6')
xtest = ctypes.CDLL('libXtst.so.6')
xlib.XOpenDisplay.restype = ctypes.c_void_p
xlib.XOpenDisplay.argtypes = [ctypes.c_char_p]
xlib.XStringToKeysym.restype = ctypes.c_ulong
xlib.XStringToKeysym.argtypes = [ctypes.c_char_p]
xlib.XKeysymToKeycode.restype = ctypes.c_ubyte
xlib.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
xlib.XFlush.argtypes = [ctypes.c_void_p]
xtest.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
display = xlib.XOpenDisplay(None)
if not display:
    raise RuntimeError('private nested display did not open')

def key(name):
    return xlib.XKeysymToKeycode(display, xlib.XStringToKeysym(name.encode()))

modifier = key('Alt_L' if backend == 'niri' else 'Super_L')
def press(name):
    code = key(name)
    for k, down in [(modifier, 1), (code, 1), (code, 0), (modifier, 0)]:
        if not xtest.XTestFakeKeyEvent(display, k, down, 0):
            raise RuntimeError('synthetic key injection failed')
    xlib.XFlush(display)

if backend == 'niri':
    ipc = socket.socket(socket.AF_UNIX)
    ipc.connect(os.environ['NIRI_SOCKET'])
    stream = ipc.makefile('rwb')
    def state():
        stream.write(b'"Windows"\n')
        stream.flush()
        response = json.loads(stream.readline())
        return response['Ok']['Windows']
else:
    def state():
        return json.loads(Path(os.environ['ROOST_COMPOSITOR_STATE']).read_text())

def capture(name):
    start = time.monotonic()
    image = ImageGrab.grab(xdisplay=os.environ['DISPLAY']).convert('RGB')
    sampled = time.monotonic()
    if image.size != (1280, 800):
        raise RuntimeError(f'inexact capture dimensions: {image.size}')
    image.save(out / (name + '.png'))
    (out / (name + '.json')).write_text(json.dumps(state(), indent=2))
    green = [x for x in range(image.width) if image.getpixel((x, 150)) == (0, 160, 64)]
    return {'capture_started': start, 'sampled': sampled,
            'green_right': max(green) + 1 if green else None}

capture('01-strip-rest')
press('r')
time.sleep(1)
capture('02-strip-preset')
samples = []
# Capture each delay in a separate repeatable transition. PNG compression and
# IPC geometry serialization cannot push the next sample past its deadline.
for ms in [0, 50, 100, 150, 200, 300, 400, 600, 800]:
    if ms:
        press('Right')
        time.sleep(0.8)
        t0 = time.monotonic()
        press('Left')
        time.sleep(max(0, t0 + ms / 1000 - time.monotonic()))
        sample = capture(f'view-{ms:03d}')
        sample['elapsed_ms'] = (sample.pop('sampled') - t0) * 1000
        sample['capture_started_ms'] = (sample.pop('capture_started') - t0) * 1000
    else:
        sample = capture('view-000')
        sample.pop('sampled')
        sample.pop('capture_started')
        sample.update(elapsed_ms=0, capture_started_ms=0)
    sample['requested_ms'] = ms
    samples.append(sample)
for src, dest in [('view-100', '03-strip-mid-scroll'), ('view-800', '04-strip-scroll-settled')]:
    for suffix in ['.png', '.json']:
        (out / (dest + suffix)).write_bytes((out / (src + suffix)).read_bytes())
(out / 'view-timing.json').write_text(json.dumps(samples, indent=2))
