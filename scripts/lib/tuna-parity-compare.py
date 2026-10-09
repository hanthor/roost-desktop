#!/usr/bin/python3
"""Compare Tuna Desktop frames against GNOME 51 reference frames, state by state.

Usage: tuna-parity-compare.py GNOME_DIR TUNA_DIR OUT_DIR [STATE[:x,y,w,h] ...]

For each state (default: every PNG in GNOME_DIR that Tuna Desktop also has),
writes OUT_DIR/<state>.png: GNOME on top, Tuna Desktop in the middle, and their
difference (x4) below, and prints the mean channel difference and the
share of pixels off by more than 24 levels. A crop limits the compare to
one element, e.g. 01-desktop:0,0,1280,32 for the top bar.
"""
import os
import sys

from PIL import Image, ImageChops

gdir, rdir, odir = sys.argv[1:4]
os.makedirs(odir, exist_ok=True)
states = sys.argv[4:] or sorted(
    f[:-4] for f in os.listdir(gdir)
    if f.endswith(".png") and os.path.exists(os.path.join(rdir, f)))
print(f"{'state':40} {'mean':>6} {'off%':>6}")
for spec in states:
    state, _, crop = spec.partition(":")
    g = Image.open(os.path.join(gdir, state + ".png")).convert("RGB")
    r = Image.open(os.path.join(rdir, state + ".png")).convert("RGB")
    if crop:
        x, y, w, h = map(int, crop.split(","))
        g, r = g.crop((x, y, x + w, y + h)), r.crop((x, y, x + w, y + h))
    if r.size != g.size:
        r = r.resize(g.size)
    diff = ImageChops.difference(g, r)
    out = Image.new("RGB", (g.width, g.height * 3 + 8), (255, 0, 255))
    out.paste(g, (0, 0))
    out.paste(r, (0, g.height + 4))
    out.paste(diff.point(lambda v: min(255, v * 4)), (0, 2 * g.height + 8))
    name = state + ("-" + crop.replace(",", "_") if crop else "")
    out.save(os.path.join(odir, name + ".png"))
    hist = diff.convert("L").histogram()
    total = g.width * g.height
    mean = sum(i * n for i, n in enumerate(hist)) / total
    off = sum(hist[25:]) / total * 100
    print(f"{name:40} {mean:6.2f} {off:6.1f}")
