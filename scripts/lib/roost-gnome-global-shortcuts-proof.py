#!/usr/bin/python3
"""Genuine GNOME51 backend/provider proof; explicit backend app ID is not frontend identity."""
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
BUS = Gio.bus_get_sync(Gio.BusType.SESSION, None)
BACKEND = 'org.freedesktop.impl.portal.desktop.gnome'
PROVIDER = 'org.gnome.Settings.GlobalShortcutsProvider'
IFACE = 'org.freedesktop.impl.portal.GlobalShortcuts'
PATH = '/org/freedesktop/portal/desktop'
EVENTS = []
RECEIPT = {'host_autorepeat': 'ordinary Xvfb default; matching Activated repeats allowed while physically held', 'scope': 'GNOME51 backend/provider only; no frontend native/Flatpak identity claim', 'events': EVENTS, 'decisions': []}


def call(destination, path, interface, method, signature=None, args=()):
    return BUS.call_sync(destination, path, interface, method,
        GLib.Variant(signature, args) if signature else None, None,
        Gio.DBusCallFlags.NONE, 5000, None).unpack()


def dbus(method, name):
    return call('org.freedesktop.DBus', '/org/freedesktop/DBus',
        'org.freedesktop.DBus', method, '(s)', (name,))[0]


def pump():
    context = GLib.MainContext.default()
    for _ in range(100):
        if not context.pending():
            break
        context.iteration(False)


def wait(predicate, message, seconds=20):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        pump()
        value = predicate()
        if value:
            return value
        time.sleep(.05)
    raise RuntimeError(message)


def observe(seconds, expected):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        pump()
        if RECEIPT.get('signal_failure'):
            raise RuntimeError(RECEIPT['signal_failure'])
        if len(EVENTS) != expected:
            raise RuntimeError('unexpected shortcut delivery during negative control')
        time.sleep(.05)


def process_identity(pid, executable):
    process = Path(f'/proc/{pid}')
    executable = executable.resolve(strict=True)
    metadata = executable.stat()
    if (process.stat().st_uid != 1000 or os.getuid() != 1000
            or (process / 'exe').resolve() != executable
            or metadata.st_uid != 0 or metadata.st_mode & 0o022
            or not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 64 * 1024 * 1024):
        raise RuntimeError('not the actual installed ordinary-user process')
    return {'pid': pid, 'uid': 1000,
        'start_ticks': (process / 'stat').read_text().rsplit(')', 1)[1].split()[19],
        'executable': str(executable), 'sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}


def identity(name, paths):
    owner = dbus('GetNameOwner', name)
    pid = dbus('GetConnectionUnixProcessID', owner)
    if dbus('GetConnectionUnixUser', owner) != 1000:
        raise RuntimeError('provider has wrong bus UID')
    actual = Path(f'/proc/{pid}/exe').resolve()
    permitted = [Path(p).resolve() for p in paths if Path(p).exists()]
    if actual not in permitted:
        raise RuntimeError('provider executable is not the installed GNOME provider')
    result = process_identity(pid, actual)
    package = subprocess.check_output(['rpm', '-qf', str(actual)], text=True, timeout=5).strip()
    if '-51.' not in package:
        raise RuntimeError('actual provider must be GNOME51: '+package)
    verification = subprocess.check_output(['rpm', '-V', package], text=True, timeout=10)
    if verification:
        raise RuntimeError('installed provider RPM verification changed')
    result.update(owner=owner, package=package, rpm_verify=verification)
    if owner != dbus('GetNameOwner', name):
        raise RuntimeError('provider owner changed during identity observation')
    return result


def pin_services(label):
    for name, key in ((BACKEND, 'backend'), (PROVIDER, 'provider')):
        observed = identity(name, [RECEIPT[key]['executable']])
        if observed != RECEIPT[key]:
            raise RuntimeError('original genuine service changed at '+label)
    version = call(RECEIPT['backend']['owner'], PATH,
        'org.freedesktop.DBus.Properties', 'Get', '(ss)', (IFACE, 'version'))[0]
    if version != RECEIPT['interface_version']:
        raise RuntimeError('original backend interface version changed at '+label)
    RECEIPT.setdefault('service_pins', []).append({'label': label, 'wall': time.time(),
        'backend': RECEIPT['backend'], 'provider': RECEIPT['provider'], 'version': version})


def provider_controls(original, label):
    desktop = pyatspi.Registry.getDesktop(0)
    apps = []
    if not 0 <= desktop.childCount <= 256:
        raise RuntimeError("accessibility desktop exceeds proof application bound")
    for index in range(desktop.childCount):
        app = desktop.getChildAtIndex(index)
        if app and app.get_process_id() == original['pid']:
            apps.append(app)
    if len(apps) > 1:
        raise RuntimeError('duplicate original provider accessibility application')
    controls, nodes = {}, []
    def walk(node, depth=0):
        if depth > 40 or len(nodes) >= 4096:
            raise RuntimeError('accessibility tree exceeds proof bounds')
        node.clearCache()
        states = node.getState()
        showing = states.contains(pyatspi.STATE_SHOWING)
        if len(node.name or '') > 1024:
            raise RuntimeError('accessible name exceeds proof bound')
        nodes.append({'name': node.name, 'role': node.getRoleName(), 'showing': showing})
        # Fedora AT-SPI reports genuine dialog buttons as 'button' where other
        # stacks report 'push button'; the retained a11y snapshot is ground truth.
        # Name, showing state, and exactly-one-each uniqueness still gate the match.
        if showing and node.getRoleName() in ('push button', 'button') and node.name in ('Add', 'Cancel'):
            controls.setdefault(node.name, []).append(node)
        if not 0 <= node.childCount <= 4096:
            raise RuntimeError("accessibility node exceeds proof child bound")
        for index in range(node.childCount):
            child = node.getChildAtIndex(index)
            if child:
                walk(child, depth+1)
    for app in apps:
        walk(app)
    (OUT / f'{label}-a11y.json').write_text(json.dumps(nodes, indent=2))
    if any(len(items) > 1 for items in controls.values()):
        raise RuntimeError('genuine provider decision controls are not unique')
    return controls if all(len(controls.get(key, [])) == 1 for key in ('Add', 'Cancel')) else None


def host(require_normal=True):
    pid = int((OUT / 'compositor.pid').read_text())
    original = process_identity(pid, Path('/candidate/usr/bin/roost-compositor'))
    windows = subprocess.check_output(['xdotool', 'search', '--onlyvisible', '--pid', str(pid),
        '--name', '^Smithay'], text=True, timeout=5).split()
    if len(windows) != 1:
        raise RuntimeError('owned nested compositor host is not unique')
    xid = windows[0]
    if int(subprocess.check_output(['xdotool', 'getwindowpid', xid], text=True, timeout=5)) != pid:
        raise RuntimeError('X host belongs to another process')
    subprocess.run(['xdotool', 'windowfocus', xid], check=True, timeout=5)
    wait(lambda: subprocess.check_output(['xdotool', 'getwindowfocus'], text=True, timeout=5).strip() == xid,
        'owned host did not receive genuine X focus')
    if process_identity(pid, Path(original['executable'])) != original:
        raise RuntimeError('host identity changed before input')
    geometry = subprocess.check_output(['xdotool', 'getwindowgeometry', '--shell', xid], text=True, timeout=5)
    fields = dict(line.split('=', 1) for line in geometry.splitlines() if '=' in line)
    if fields.get('WIDTH') != '1280' or fields.get('HEIGHT') != '800':
        raise RuntimeError('actual owned host dimensions changed')
    original.update(xid=xid, focus=xid, geometry=fields)
    if RECEIPT.get('host') and RECEIPT['host'] != original:
        raise RuntimeError('original owned host changed between genuine input events')
    RECEIPT['host'] = original
    (OUT / 'host-identity.json').write_text(json.dumps(original, indent=2))
    scene = json.loads((OUT / 'compositor-state.json').read_text())
    if require_normal and (scene.get('locked') or scene.get('overview_open')):
        raise RuntimeError('genuine shortcut probe requires actual normal unlocked mode')
    RECEIPT.setdefault('input_preconditions', []).append({'wall': time.time(),
        'focused_window': scene.get('focused'), 'focused_app_id': scene.get('focused_app_id'),
        'overview_open': scene.get('overview_open'), 'locked': scene.get('locked'), 'host': original})
    return xid


def key_command(command, key):
    host()
    pin_services('before-'+command)
    subprocess.run(['xdotool', command, key], check=True, timeout=5)
    stamp = {'command': command, 'wall': time.time(), 'monotonic': time.monotonic()}
    RECEIPT.setdefault('input_commands', []).append(stamp)
    pin_services('after-'+command)
    return stamp


def signal(connection, sender, path, interface, name, parameters, user):
    session, shortcut, timestamp, options = parameters.unpack()
    if len(EVENTS) >= 512:
        RECEIPT['signal_failure'] = 'actual shortcut event count exceeds retained proof bound'
        return
    EVENTS.append({'signal': name, 'session': session, 'shortcut': shortcut,
        'timestamp': timestamp, 'options': options, 'wall': time.time()})


def matching_sequence(session, allow_release):
    if RECEIPT.get('signal_failure'):
        raise RuntimeError(RECEIPT['signal_failure'])
    activated, deactivated = 0, 0
    for event in EVENTS:
        if event['session'] != session or event['shortcut'] != 'activate-proof':
            raise RuntimeError('shortcut event belongs to a different original session or shortcut')
        if event['signal'] == 'Activated' and deactivated == 0:
            activated += 1
        elif event['signal'] == 'Deactivated' and allow_release and activated > 0:
            deactivated += 1
            if deactivated > 1:
                raise RuntimeError('original held press emitted multiple releases')
        else:
            raise RuntimeError('unexpected release while held or activation after release')
    return activated, deactivated


def decision(kind, number):
    pin_services('before-'+kind)
    app_id = f'org.tuna.GlobalShortcutProof{number}'
    root = '/org/freedesktop/portal/desktop'
    request = f'{root}/request/gnome_shortcut_proof/{number}'
    session = f'{root}/session/gnome_shortcut_proof/{number}'
    response, results = call(RECEIPT['backend']['owner'], PATH, IFACE, 'CreateSession',
        '(oosa{sv})', (request, session, app_id, {}))
    if response != 0 or results.get('session_id') != session:
        raise RuntimeError('actual GNOME backend did not create the requested session')
    pending = []
    def complete(connection, result, unused):
        try:
            pending.append(('response', connection.call_finish(result).unpack()))
        except GLib.Error as error:
            pending.append(('error', error.message))
    BUS.call(RECEIPT['backend']['owner'], PATH, IFACE, 'BindShortcuts',
        GLib.Variant('(ooa(sa{sv})sa{sv})', (request, session,
            [('activate-proof', {'description': GLib.Variant('s', 'Shortcut proof'),
                'preferred_trigger': GLib.Variant('s', 'CTRL+ALT+F8')})], '', {})),
        None, Gio.DBusCallFlags.NONE, 45000, None, complete, None)
    controls = wait(lambda: provider_controls(RECEIPT['provider'], kind),
        'actual GNOME51 Add/Cancel controls did not appear', 30)
    if pending:
        raise RuntimeError('BindShortcuts completed before the real decision')
    subprocess.run(['scrot', '-o', str(OUT / f'{kind}-before.png')], check=True, timeout=5)
    action = controls['Cancel' if kind == 'cancel' else 'Add'][0].queryAction()
    indexes = [i for i in range(action.nActions) if action.getName(i) in ('click', 'activate', 'press')]
    if len(indexes) != 1 or not action.doAction(indexes[0]):
        raise RuntimeError('actual unique provider decision action failed')
    wait(lambda: pending, 'real GNOME shortcut decision did not complete', 20)
    if pending[0][0] != 'response':
        raise RuntimeError(pending)
    response, result = pending[0][1]
    pin_services('after-'+kind+'-response')
    expected = 2 if kind == 'cancel' else 0
    if response != expected:
        raise RuntimeError(('actual GNOME backend decision differs', response, expected))
    schema = 'org.gnome.settings-daemon.global-shortcuts.application'
    settings_path = f'/org/gnome/settings-daemon/global-shortcuts/{app_id}/'
    persisted = subprocess.check_output(['gsettings', 'get', schema+':'+settings_path,
        'shortcuts'], text=True, timeout=5).strip()
    RECEIPT['decisions'].append({'decision': kind, 'app_id': app_id, 'session': session,
        'response': response, 'result': result, 'persisted': persisted})
    if kind == 'cancel':
        if result.get('shortcuts') or 'activate-proof' in persisted:
            raise RuntimeError('denied dialog retained a shortcut grant')
        key_command('key', 'ctrl+alt+F8')
        observe(1, 0)
    else:
        if len(result.get('shortcuts', [])) != 1 or 'activate-proof' not in persisted:
            raise RuntimeError('Add did not persist actual GNOME shortcut metadata')
        try:
            down = key_command('keydown', 'ctrl+alt+F8')
            wait(lambda: matching_sequence(session, False)[0] > 0,
                'actual held shortcut did not activate', 5)
            end = time.monotonic() + .5
            while time.monotonic() < end:
                pump()
                matching_sequence(session, False)
                if time.monotonic() - down['monotonic'] > 15:
                    raise RuntimeError('physical held interval exceeded the bounded proof deadline')
                time.sleep(.05)
            RECEIPT['held_observation'] = {'activated_count': matching_sequence(session, False)[0],
                'deactivated_count': 0, 'wall': time.time(), 'monotonic': time.monotonic()}
        except BaseException as error:
            RECEIPT['held_failure'] = {'type': type(error).__name__, 'message': str(error)[:2048]}
            raise
        finally:
            # Always release only this original owned host, even if the
            # GNOME service failed after receiving a partial keydown.
            try:
                host(require_normal=False)
                subprocess.run(['xdotool', 'keyup', 'ctrl+alt+F8'], check=True, timeout=5)
                RECEIPT['keyup'] = {'wall': time.time(), 'monotonic': time.monotonic()}
                RECEIPT['held_cleanup'] = 'original-host-modifiers-released'
            except BaseException as error:
                RECEIPT['held_cleanup_failure'] = {'type': type(error).__name__, 'message': str(error)[:2048]}
                raise
        pin_services('after-keyup')
        duration = RECEIPT['keyup']['monotonic'] - down['monotonic']
        if duration > 15:
            raise RuntimeError('actual keydown-to-keyup interval exceeded proof bound')
        wait(lambda: matching_sequence(session, True)[1] == 1,
            'actual released shortcut did not deactivate', 5)
        activated, deactivated = matching_sequence(session, True)
        RECEIPT['physical_sequence'] = {'activated_count': activated,
            'deactivated_count': deactivated, 'held_seconds': duration}
        observe(.5, len(EVENTS))
        matching_sequence(session, True)
    pin_services('after-'+kind+'-chord')
    call(RECEIPT['backend']['owner'], session, 'org.freedesktop.impl.portal.Session', 'Close')
    pin_services('after-'+kind+'-Close')
    expected_events = len(EVENTS)
    key_command('key', 'ctrl+alt+F8')
    observe(1, expected_events)
    pin_services('after-'+kind+'-Close-negative')


try:
    wait(lambda: dbus('NameHasOwner', BACKEND), 'real GNOME backend did not start')
    RECEIPT['backend'] = identity(BACKEND, ['/usr/libexec/xdg-desktop-portal-gnome', '/usr/lib/xdg-desktop-portal-gnome'])
    wait(lambda: dbus('NameHasOwner', PROVIDER), 'real GNOME provider did not start')
    RECEIPT['provider'] = identity(PROVIDER, ['/usr/libexec/gnome-control-center-global-shortcuts-provider', '/usr/lib/gnome-control-center-global-shortcuts-provider'])
    version = call(RECEIPT['backend']['owner'], PATH, 'org.freedesktop.DBus.Properties', 'Get',
        '(ss)', (IFACE, 'version'))[0]
    if version < 2:
        raise RuntimeError('GNOME GlobalShortcuts version2 required')
    RECEIPT['interface_version'] = version
    for name in ('Activated', 'Deactivated'):
        BUS.signal_subscribe(RECEIPT['backend']['owner'], IFACE, name, PATH, None,
            Gio.DBusSignalFlags.NONE, signal, None)
    decision('cancel', 1)
    decision('add', 2)
    RECEIPT['pass'] = True
except BaseException as error:
    RECEIPT['failure'] = {'type': type(error).__name__, 'message': str(error)[:2048]}
    raise
finally:
    (OUT / 'global-shortcuts-receipt.json').write_text(json.dumps(RECEIPT, indent=2))
