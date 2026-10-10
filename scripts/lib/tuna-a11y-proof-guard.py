#!/usr/bin/python3
"""Retain pidfds for daemons owned by one private accessibility proof bus."""
import json
import os
from pathlib import Path
import select
import signal
import sys
import time

from gi.repository import Gio, GLib

address, artifacts = sys.argv[1:]
artifacts = Path(artifacts)
connection = Gio.DBusConnection.new_for_address_sync(
    address,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
    | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None,
    None,
)

def call(method, parameters):
    return connection.call_sync(
        "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
        method, parameters, None, Gio.DBusCallFlags.NO_AUTO_START, 3000, None,
    ).unpack()[0]

# Start the registry explicitly on our private bus, so all owned services
# have retained pidfds before the test starts. Query failure refuses proof.
call("StartServiceByName", GLib.Variant("(su)", ("org.a11y.atspi.Registry", 0)))
owned = []
try:
    for name in ["org.a11y.atspi.Registry", "org.freedesktop.DBus"]:
        pid = call("GetConnectionUnixProcessID", GLib.Variant("(s)", (name,)))
        fd = os.pidfd_open(pid)
        owned.append((name, pid, fd))
    identity = {
        "address": address,
        "owned": [{"service": name, "pid": pid, "pidfd_retained": True}
                  for name, pid, _ in owned],
        "exit_verified": False,
    }
    (artifacts / "owned-accessibility.json").write_text(json.dumps(identity, indent=2) + "\n")
    (artifacts / "guard-ready").write_text("ready\n")
    while not (artifacts / "guard-stop").exists():
        time.sleep(.05)
    # A live DBus connection must not keep the daemon open during shutdown.
    connection.close_sync(None)
    for name, pid, fd in owned:
        poll = select.poll()
        poll.register(fd, select.POLLIN)
        if not poll.poll(0):
            signal.pidfd_send_signal(fd, signal.SIGTERM)
        if not poll.poll(3000):
            signal.pidfd_send_signal(fd, signal.SIGKILL)
            if not poll.poll(3000):
                raise RuntimeError(f"owned service did not exit: {name} pid={pid}")
    identity["exit_verified"] = True
    (artifacts / "owned-accessibility.json").write_text(json.dumps(identity, indent=2) + "\n")
finally:
    for _, _, fd in owned:
        os.close(fd)
