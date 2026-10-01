#!/usr/bin/env python3
"""Dump one application's AT-SPI accessible tree as JSON (#71).

Usage: roost-a11y-dump.py APP_NAME OUT.json [TIMEOUT_S]

Each node: role, name, depth, showing. Waits up to TIMEOUT_S for the app
to register on the accessibility bus. Exit 1 if it never appears.
"""
import json
import sys
import time

import pyatspi


def walk(acc, depth, out):
    try:
        out.append({
            "role": acc.getRoleName(),
            "name": acc.name or "",
            "depth": depth,
            "showing": acc.getState().contains(pyatspi.STATE_SHOWING),
        })
        for i in range(acc.childCount):
            child = acc.getChildAtIndex(i)
            if child is not None:
                walk(child, depth + 1, out)
    except Exception as exc:  # a node can die mid-walk; keep the rest
        out.append({"role": "error", "name": str(exc), "depth": depth, "showing": False})


def main():
    name, path = sys.argv[1], sys.argv[2]
    timeout = float(sys.argv[3]) if len(sys.argv) > 3 else 20
    end = time.time() + timeout
    while time.time() < end:
        desktop = pyatspi.Registry.getDesktop(0)
        for i in range(desktop.childCount):
            app = desktop.getChildAtIndex(i)
            if app is not None and app.name == name:
                nodes = []
                walk(app, 0, nodes)
                with open(path, "w") as fh:
                    json.dump(nodes, fh, indent=1)
                print(f"roost-a11y-dump: {len(nodes)} nodes from {name}")
                return 0
        time.sleep(0.5)
    print(f"roost-a11y-dump: {name} never registered on the a11y bus", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
