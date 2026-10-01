#!/usr/bin/env python3
"""Dump one application's AT-SPI accessible tree as JSON (#71).

Usage: roost-a11y-dump.py APP_NAME OUT.json [TIMEOUT_S] [--press NAME]
                          [--set NAME VALUE]

With --press, also performs the first action (click) of the first
showing node named NAME, as a screen-reader user would, then dumps.
With --set, sets the value of the first showing node named NAME that
has one (a slider), the way assistive technology moves it.

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
            "checked": acc.getState().contains(pyatspi.STATE_CHECKED)
            or acc.getState().contains(pyatspi.STATE_PRESSED),
            "selected": acc.getState().contains(pyatspi.STATE_SELECTED),
        })
        for i in range(acc.childCount):
            child = acc.getChildAtIndex(i)
            if child is not None:
                walk(child, depth + 1, out)
    except Exception as exc:  # a node can die mid-walk; keep the rest
        out.append({"role": "error", "name": str(exc), "depth": depth, "showing": False})


def press(acc, target):
    try:
        if acc.name == target and acc.getState().contains(pyatspi.STATE_SHOWING):
            action = acc.queryAction()
            if action.nActions > 0:
                action.doAction(0)
                return True
        for i in range(acc.childCount):
            child = acc.getChildAtIndex(i)
            if child is not None and press(child, target):
                return True
    except Exception:
        pass
    return False


def set_value(acc, target, value):
    try:
        if acc.name == target and acc.getState().contains(pyatspi.STATE_SHOWING):
            try:
                acc.queryValue().currentValue = value
                return True
            except NotImplementedError:
                pass
        for i in range(acc.childCount):
            child = acc.getChildAtIndex(i)
            if child is not None and set_value(child, target, value):
                return True
    except Exception:
        pass
    return False


def main():
    args = sys.argv[1:]
    target = None
    setting = None
    if "--press" in args:
        i = args.index("--press")
        target = args[i + 1]
        del args[i:i + 2]
    if "--set" in args:
        i = args.index("--set")
        setting = (args[i + 1], float(args[i + 2]))
        del args[i:i + 3]
    name, path = args[0], args[1]
    timeout = float(args[2]) if len(args) > 2 else 20
    end = time.time() + timeout
    while time.time() < end:
        desktop = pyatspi.Registry.getDesktop(0)
        for i in range(desktop.childCount):
            app = desktop.getChildAtIndex(i)
            if app is not None and app.name == name:
                if target is not None:
                    if not press(app, target):
                        print(f"roost-a11y-dump: nothing named {target} to press", file=sys.stderr)
                        return 1
                    time.sleep(1.5)
                if setting is not None:
                    if not set_value(app, *setting):
                        print(f"roost-a11y-dump: nothing named {setting[0]} to set", file=sys.stderr)
                        return 1
                    time.sleep(1.5)
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
