#!/usr/bin/env python3
"""Re-render the Roost VM tour title cards (needs Pillow and DejaVu Sans).

The PNGs beside this file are committed; CI never runs this. Run it after
changing a card's text:  python3 scripts/lib/vm-tour-cards/render.py
"""
import os

from PIL import Image, ImageDraw, ImageFont

CARDS = {
    "intro": ("Roost on TunaOS Marlin",
              "GNOME 51-style desktop in Rust, booted in QEMU from the bootc image"),
    "overview": ("Activities overview", "Super opens the overview; typing searches apps"),
    "app-grid": ("App grid", "Super+A shows every installed application"),
    "quick-settings": ("Quick settings", "The status area opens the quick settings menu"),
    "calendar": ("Calendar and notifications", "The clock opens the calendar and message list"),
    "alt-tab": ("Windows and Alt+Tab", "Open a few apps, then switch with Alt+Tab"),
    "scroll": ("Scrollable tiling",
               "Super+Shift+T toggles scroll mode, Super+R resizes, Super+Left/Right moves"),
    "lock": ("Lock screen", "Lock from quick settings, then unlock with the password"),
}
W, H = 1280, 800
FONTS = "/usr/share/fonts/truetype/dejavu"


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    big = ImageFont.truetype(os.path.join(FONTS, "DejaVuSans-Bold.ttf"), 64)
    small = ImageFont.truetype(os.path.join(FONTS, "DejaVuSans.ttf"), 28)
    for name, (title, sub) in CARDS.items():
        im = Image.new("RGB", (W, H), (24, 24, 24))
        d = ImageDraw.Draw(im)
        d.rectangle([0, 0, W, 32], fill=(0, 0, 0))
        d.text((W // 2, H // 2 - 30), title, font=big, fill=(255, 255, 255), anchor="mm")
        d.text((W // 2, H // 2 + 50), sub, font=small, fill=(190, 190, 190), anchor="mm")
        im = im.convert("P", palette=Image.ADAPTIVE, colors=16)
        im.save(os.path.join(here, f"{name}.png"), optimize=True)


if __name__ == "__main__":
    main()
