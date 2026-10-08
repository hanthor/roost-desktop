#!/usr/bin/python3
"""Temporary CI diagnostic: show which (keycode, level) pairs carry a
keysym on an X server, exactly as xdotool's keysym lookup sees it.
Usage: xkeysym-levels.py DISPLAY F1 [keysyms...] (hex, e.g. ffbe)."""
import ctypes
import sys


def main():
    display = sys.argv[1].encode()
    x11 = ctypes.CDLL("libX11.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XKeycodeToKeysym.restype = ctypes.c_ulong
    x11.XKeycodeToKeysym.argtypes = [ctypes.c_void_p, ctypes.c_ubyte, ctypes.c_int]
    dpy = x11.XOpenDisplay(display)
    if not dpy:
        print("cannot open display %s" % sys.argv[1])
        return 1
    try:
        for raw in sys.argv[2:]:
            want = int(raw, 16)
            hits = []
            for level in range(8):
                for code in range(8, 256):
                    if x11.XKeycodeToKeysym(dpy, code, level) == want:
                        hits.append((code, level))
            print("keysym 0x%04x found at (keycode, level): %s" % (want, hits))
    finally:
        x11.XCloseDisplay(dpy)
    return 0


if __name__ == "__main__":
    sys.exit(main())
