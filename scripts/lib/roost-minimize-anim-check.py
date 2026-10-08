#!/usr/bin/env python3
"""Watch one minimize or restore animation through the compositor state file.

Usage: roost-minimize-anim-check.py STATE WINDOW DIRECTION OUT
           (--animated ICON_JSON | --fade | --instant) [--timeout S]

Start it before the key or click that minimizes (or restores) WINDOW. It
samples ROOST_COMPOSITOR_STATE (rewritten every frame that changes it)
until a new `minimize_settled` entry for WINDOW and DIRECTION appears,
writes every sample plus the settled entry to OUT, and checks GNOME 51's
shape (windowManager.js `_minimizeWindow`/`_unminimizeWindow`):

  --animated ICON  the animation ran: minimize ends exactly on ICON (the
                   dash tile the shell published) and every drawn frame
                   is closer to it than the one before; restore starts
                   on ICON and grows away from it; it settles within
                   400 ms plus a frame-scheduling tolerance.
  --fade           reduced motion (fade-only): it animated in place, the
                   window never moving or scaling, only its opacity.
  --instant        animations off: it settled without animating and no
                   frame drew it in flight.
"""

import argparse
import json
import math
import sys
import time

DURATION_MS = 400.0
# The animation clock settles on the first frame at or after 400 ms; a
# loaded software-rendered CI runner can draw that frame late.
TOLERANCE_MS = 250.0


def load(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def center(rect):
    return (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0)


def close(a, b, eps=1.0):
    return all(abs(x - y) <= eps for x, y in zip(a, b))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("state")
    ap.add_argument("window", type=int)
    ap.add_argument("direction", choices=["minimize", "restore"])
    ap.add_argument("out")
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--animated", metavar="ICON_JSON")
    mode.add_argument("--fade", action="store_true")
    mode.add_argument("--instant", action="store_true")
    ap.add_argument("--timeout", type=float, default=20.0)
    args = ap.parse_args()

    def mine(entry):
        return entry.get("window") == args.window and entry.get("direction") == args.direction

    first = load(args.state) or {}
    seen = sum(1 for s in first.get("minimize_settled", []) if mine(s))
    samples, settled = [], None
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        doc = load(args.state)
        if doc is not None:
            for a in doc.get("minimize_animations", []):
                if mine(a) and (not samples or samples[-1] != a):
                    samples.append(a)
            done = [s for s in doc.get("minimize_settled", []) if mine(s)]
            if len(done) > seen:
                settled = done[-1]
                break
        time.sleep(0.005)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump({"samples": samples, "settled": settled}, f, indent=1)
    if settled is None:
        sys.exit(f"no settled {args.direction} for window {args.window}")

    if args.instant:
        if settled["animated"] or samples:
            sys.exit(f"animations off still animated: {settled}, {len(samples)} frames")
        print(f"{args.direction} snapped with animations off")
        return

    if not settled["animated"]:
        sys.exit(f"did not animate: {settled}")
    if args.fade:
        moved = [s for s in samples if not close(s["rect"], settled["to"], 0.01)]
        if not close(settled["from"], settled["to"], 0.01) or moved:
            sys.exit(f"fade-only {args.direction} moved: {settled}, {len(moved)} frames")
        alphas = [s["alpha"] for s in samples if 0.0 < s["progress"] < 1.0]
        if not alphas or not all(0.0 < a < 1.0 for a in alphas):
            sys.exit(f"fade-only {args.direction} never drew a partial fade: {alphas}")
        print(f"{args.direction} faded in place over {len(alphas)} frames")
        return
    icon = json.loads(args.animated)
    if settled["elapsed_ms"] > DURATION_MS + TOLERANCE_MS:
        sys.exit(f"settled after {settled['elapsed_ms']:.0f} ms")
    if settled["elapsed_ms"] < DURATION_MS:
        sys.exit(f"settled early, after {settled['elapsed_ms']:.0f} ms")
    end = settled["to"] if args.direction == "minimize" else settled["from"]
    if not close(end, icon):
        sys.exit(f"{args.direction} {end} is not the dash icon {icon}")
    moving = [s for s in samples if 0.0 < s["progress"] < 1.0]
    if not moving:
        sys.exit("no frame drew the window in flight")
    target = center(icon)
    dist = [math.dist(center(s["rect"]), target) for s in moving]
    widths = [s["rect"][2] for s in moving]
    if args.direction == "minimize":
        heads = all(b <= a + 0.5 for a, b in zip(dist, dist[1:]))
        shrinks = all(b <= a + 0.5 for a, b in zip(widths, widths[1:]))
        fades = all(s["alpha"] < 1.0 for s in moving)
    else:
        heads = all(b >= a - 0.5 for a, b in zip(dist, dist[1:]))
        shrinks = all(b >= a - 0.5 for a, b in zip(widths, widths[1:]))
        fades = all(s["alpha"] == 1.0 for s in moving)
    if not (heads and shrinks and fades):
        sys.exit(f"{args.direction} frames are not GNOME's: dist {dist} widths {widths}")
    print(
        f"{args.direction}: {len(moving)} frames toward the dash icon, "
        f"settled after {settled['elapsed_ms']:.0f} ms"
    )


if __name__ == "__main__":
    main()
