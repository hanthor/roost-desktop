#!/usr/bin/python3
"""Tap one X key by raw keycode through XTEST, with no keysym lookup.

xdotool(1) `key F1` resolves function keys behind an Alt level on the
proof server's keymap and taps Alt around every press, which poisons a
Super+F1 cycle chord (the compositor logs a near-miss with mods=72 and
no cycle happens). Pressing the level-0 keycode directly keeps the chord
to exactly the modifiers the harness holds down itself.

Usage: xkeycode-tap.py DISPLAY KEYSYM_HEX (e.g. :96 ffbe).
Exits nonzero when the display cannot be opened, no level-0 keycode
carries the keysym, or the XTEST tap fails.
"""
import ctypes
import sys
import time


def main():
    if len(sys.argv) != 3:
        print("usage: xkeycode-tap.py DISPLAY KEYSYM_HEX", file=sys.stderr)
        return 2
    display = sys.argv[1].encode()
    try:
        want = int(sys.argv[2], 16)
    except ValueError:
        print("bad keysym %r" % sys.argv[2], file=sys.stderr)
        return 2
    x11 = ctypes.CDLL("libX11.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XKeycodeToKeysym.restype = ctypes.c_ulong
    x11.XKeycodeToKeysym.argtypes = [ctypes.c_void_p, ctypes.c_ubyte, ctypes.c_int]
    x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    xtst = ctypes.CDLL("libXtst.so.6")
    xtst.XTestFakeKeyEvent.restype = ctypes.c_int
    xtst.XTestFakeKeyEvent.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint,
        ctypes.c_int,
        ctypes.c_ulong,
    ]
    dpy = x11.XOpenDisplay(display)
    if not dpy:
        print("cannot open display %s" % sys.argv[1], file=sys.stderr)
        return 1
    try:
        code = 0
        for candidate in range(8, 256):
            if x11.XKeycodeToKeysym(dpy, candidate, 0) == want:
                code = candidate
                break
        if not code:
            print(
                "keysym 0x%04x has no level-0 keycode" % want, file=sys.stderr
            )
            return 1
        if not xtst.XTestFakeKeyEvent(dpy, code, 1, 0):
            print("XTEST press for keycode %d failed" % code, file=sys.stderr)
            return 1
        x11.XSync(dpy, 0)
        time.sleep(0.05)
        if not xtst.XTestFakeKeyEvent(dpy, code, 0, 0):
            print("XTEST release for keycode %d failed" % code, file=sys.stderr)
            return 1
        x11.XSync(dpy, 0)
    finally:
        x11.XCloseDisplay(dpy)
    return 0


if __name__ == "__main__":
    sys.exit(main())
