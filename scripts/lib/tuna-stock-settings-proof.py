#!/usr/bin/python3
"""Qualify one real GNOME51 Settings control; never write its GSettings key."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import time

import pyatspi
from gi.repository import Gio, GLib

OUT = Path('/out')
STATE = OUT / 'compositor-state.json'
BUS = Gio.bus_get_sync(Gio.BusType.SESSION, None)
NAME = 'org.gnome.Settings'
EXE = Path('/usr/bin/gnome-control-center')
steps = []


def bus_call(method, signature, args):
    return BUS.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                         'org.freedesktop.DBus', method,
                         GLib.Variant(signature, args), None,
                         Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]


def drain():
    while GLib.MainContext.default().pending():
        GLib.MainContext.default().iteration(False)


def wait_for(predicate, label, seconds=15):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        drain()
        if predicate():
            return
        time.sleep(.1)
    raise RuntimeError(label)


def scene():
    return json.loads(STATE.read_text())


def key():
    # A fresh independent process reads the shared backend, without a setter.
    value = subprocess.check_output(['gsettings', 'get',
        'org.gnome.desktop.interface', 'enable-hot-corners'], text=True).strip()
    if value not in ('true', 'false'):
        raise RuntimeError('unexpected hot-corner value')
    return value == 'true'


def identity(process):
    owner = bus_call('GetNameOwner', '(s)', (NAME,))
    pid = bus_call('GetConnectionUnixProcessID', '(s)', (owner,))
    uid = bus_call('GetConnectionUnixUser', '(s)', (owner,))
    metadata = EXE.stat()
    if (pid != process.pid or uid != os.getuid() or uid != 1000
            or Path(f'/proc/{pid}/exe').resolve() != EXE.resolve()
            or metadata.st_uid != 0 or metadata.st_mode & 0o022
            or not stat.S_ISREG(metadata.st_mode)):
        raise RuntimeError('Settings identity is not the actual installed ordinary-user app')
    proc_stat = Path(f'/proc/{pid}/stat').read_text()
    start_ticks = proc_stat.rsplit(')', 1)[1].split()[19]
    return {'owner': owner, 'pid': pid, 'uid': uid, 'start_ticks': start_ticks,
            'executable': str(EXE.resolve()), 'bytes': metadata.st_size,
            'sha256': hashlib.sha256(EXE.read_bytes()).hexdigest()}


def guard(process, original):
    if process.poll() is not None or identity(process) != original:
        raise RuntimeError('original Settings identity changed during the journey')


def walk(node, nodes, controls, depth=0):
    if depth > 32 or len(nodes) >= 4096:
        raise RuntimeError('Settings accessible tree exceeded its bound')
    node.clear_cache()
    state = node.getState()
    name, role = node.name or '', node.getRoleName()
    showing = state.contains(pyatspi.STATE_SHOWING)
    checked = state.contains(pyatspi.STATE_CHECKED)
    nodes.append({'name': name, 'role': role, 'depth': depth,
                  'showing': showing, 'checked': checked,
                  'sensitive': state.contains(pyatspi.STATE_SENSITIVE)})
    if (name.replace('_', '') == 'Hot Corner' and showing
            and state.contains(pyatspi.STATE_SENSITIVE)
            and role in ('toggle button', 'check box', 'switch')):
        controls.append(node)
    for index in range(node.childCount):
        child = node.getChildAtIndex(index)
        if child is not None:
            walk(child, nodes, controls, depth + 1)


def control(process, original, label):
    guard(process, original)
    nodes, controls = [], []
    desktop = pyatspi.Registry.getDesktop(0)
    apps = []
    for index in range(desktop.childCount):
        app = desktop.getChildAtIndex(index)
        if app is not None and app.get_process_id() == original['pid']:
            apps.append(app)
    if len(apps) > 1:
        raise RuntimeError('duplicate Settings accessibility application')
    if apps:
        walk(apps[0], nodes, controls)
    (OUT / f'{label}-a11y.json').write_text(json.dumps(nodes, indent=2))
    if len(controls) > 1:
        raise RuntimeError('Hot Corner switch is not unique')
    return controls[0] if controls else None


def launch(label):
    log = (OUT / f'{label}-settings.log').open('w')
    process = subprocess.Popen([str(EXE), 'multitasking'], stdout=log, stderr=log)
    try:
        wait_for(lambda: bus_call('NameHasOwner', '(s)', (NAME,)), 'Settings did not own its bus name')
        original = identity(process)
        (OUT / f'{label}-identity.json').write_text(json.dumps(original, indent=2))
        wait_for(lambda: control(process, original, label) is not None,
                 'actual Settings Hot Corner switch did not appear', 30)
        wait_for(lambda: scene().get('focused_app_id') == NAME,
                 'actual Settings did not map and receive compositor focus')
        return process, original
    except BaseException:
        process.terminate()
        process.wait(timeout=10)
        raise
    finally:
        log.close()


def snapshot(label):
    subprocess.run(['scrot', '-o', str(OUT / f'{label}.png')], check=True)
    current = scene()
    (OUT / f'{label}-scene.json').write_text(json.dumps(current, indent=2))
    return current


def pointer(x, y):
    windows = subprocess.check_output(['xdotool', 'search', '--onlyvisible',
                                       '--name', '^Smithay'], text=True).split()
    if len(windows) != 1:
        raise RuntimeError('expected one actual nested compositor host')
    subprocess.run(['xdotool', 'windowfocus', '--sync', windows[0]], check=True)
    subprocess.run(['xdotool', 'mousemove', '--window', windows[0], str(x), str(y)], check=True)


def corner(expected, label):
    pointer(640, 400)
    wait_for(lambda: not scene()['overview_open'], 'overview was already open before corner probe')
    pointer(1, 1)
    if expected:
        wait_for(lambda: scene()['overview_open'], 'enabled UI setting did not open actual overview')
        snapshot(label + '-overview')
        pointer(640, 400)
        subprocess.run(['xdotool', 'key', 'Escape'], check=True)
        wait_for(lambda: not scene()['overview_open'], 'Escape did not dismiss actual overview')
    else:
        end = time.monotonic() + 2
        observations = 0
        while time.monotonic() < end:
            if scene()['overview_open']:
                raise RuntimeError('disabled UI setting still opened the overview')
            observations += 1
            time.sleep(.1)
        if observations < 10:
            raise RuntimeError('insufficient disabled-corner observations')
    snapshot(label + '-desktop')
    pointer(640, 400)


def toggle(process, original, desired, label):
    switch = control(process, original, label + '-before')
    if switch is None or key() == desired:
        raise RuntimeError('missing switch or no actual state transition to prove')
    if switch.getState().contains(pyatspi.STATE_CHECKED) != key():
        raise RuntimeError('Settings switch disagrees with independent persisted key')
    action = switch.queryAction()
    if action.nActions != 1 or not action.doAction(0):
        raise RuntimeError('actual Hot Corner switch did not accept its accessibility action')
    wait_for(lambda: key() == desired, 'Settings UI did not change independent shared key')
    def updated_switch():
        current = control(process, original, label + '-after')
        return current is not None and current.getState().contains(pyatspi.STATE_CHECKED) == desired
    wait_for(updated_switch, 'actual switch did not update its checked state')
    # Let the real GTK shell's settings monitor publish to the compositor.
    # The corner result below, rather than this delay, is the acceptance proof.
    time.sleep(1)
    guard(process, original)
    persisted = OUT / 'config/glib-2.0/settings/keyfile'
    if not persisted.is_file():
        raise RuntimeError('shared keyfile backend did not persist the UI setting')
    (OUT / f'{label}-settings-keyfile.txt').write_bytes(persisted.read_bytes())
    corner(desired, label)
    steps.append({'label': label, 'identity': original, 'enabled': desired,
                  'setting_changed_by': 'actual AT-SPI Hot Corner switch action'})
    (OUT / 'journey.json').write_text(json.dumps(steps, indent=2))


process = None
try:
    if not key():
        raise RuntimeError('fresh GNOME51 fixture did not start with default enabled hot corners')
    process, original = launch('initial')
    toggle(process, original, False, 'disabled')
    toggle(process, original, True, 'enabled')
    toggle(process, original, False, 'disabled-before-restart')
    guard(process, original)
    process.terminate()
    process.wait(timeout=10)
    wait_for(lambda: not bus_call('NameHasOwner', '(s)', (NAME,)), 'old Settings bus owner survived exit')
    process, restarted = launch('restarted')
    if restarted['pid'] == original['pid'] or restarted['owner'] == original['owner']:
        raise RuntimeError('Settings did not genuinely restart')
    switch = control(process, restarted, 'restarted-persisted')
    if key() or switch.getState().contains(pyatspi.STATE_CHECKED):
        raise RuntimeError('disabled setting did not survive real Settings restart')
    corner(False, 'restarted-disabled')
    toggle(process, restarted, True, 'restarted-enabled')
finally:
    if process is not None and process.poll() is None:
        process.terminate()
        process.wait(timeout=10)
