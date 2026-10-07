#!/usr/bin/python3
"""Stage-one genuine GNOME51 Night Light journey; no GSettings setters/stubs."""
import ctypes
import datetime
import signal
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import time
import sys

import pyatspi
from gi.repository import Gio, GLib, GdkPixbuf

OUT = Path('/out')
BUS = Gio.bus_get_sync(Gio.BusType.SESSION, None)
STATE = OUT / 'compositor-state.json'
COLOR = 'org.gnome.SettingsDaemon.Color'
SETTINGS = 'org.gnome.Settings'
DISPLAY_CONFIG = 'org.gnome.Mutter.DisplayConfig'
DAEMON = Path('/usr/libexec/gsd-color')
CC = Path('/usr/bin/gnome-control-center')
FIXTURE_NEGATIVE = sys.argv[1:] == ['--fixture-negative']
if sys.argv[1:] and not FIXTURE_NEGATIVE:
    raise RuntimeError('unknown fixture arguments')
COMPOSITOR = Path('/usr/libexec/roost-vm-night-light-compositor' if FIXTURE_NEGATIVE else '/candidate/usr/bin/roost-compositor')
COMPOSITOR_PID = int(os.environ['ROOST_NIGHT_LIGHT_COMPOSITOR_PID'])
records = []
STATIC_ID = None
STATIC_COLORS = [[180,120,100],[100,180,120],[120,100,180],[160,160,100],[100,160,160]]
STATIC_PROBES = [(40,80+160*i) for i in range(5)]


def command(args):
    return subprocess.check_output(args, text=True, timeout=5).strip()


def bounded(path, limit=64 * 1024 * 1024, proc=False):
    with path.open('rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or not (0 <= before.st_size <= limit if proc else 0 < before.st_size <= limit):
            raise RuntimeError('invalid bounded regular resource: '+str(path))
        value = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
    named = path.stat()
    key = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns)
    if len(value) > limit or key(before) != key(after) or key(after) != key(named):
        raise RuntimeError('resource changed during bounded observation')
    return value


def original_state_receipt():
    fd = os.open(STATE, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd,'rb') as stream:
        before=os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_uid != 1000 or not 0 < before.st_size <= 1024*1024:
            raise RuntimeError('actual original state resource bounds')
        raw=stream.read(1024*1024+1)
        after=os.fstat(stream.fileno())
    named=STATE.lstat()
    key=lambda item:(item.st_dev,item.st_ino,item.st_uid,item.st_mode,item.st_nlink,
                     item.st_size,item.st_mtime_ns,item.st_ctime_ns)
    if len(raw)>1024*1024 or key(before)!=key(after) or key(after)!=key(named):
        raise RuntimeError('actual original state replaced during independent receipt')
    return raw, {'sha256':hashlib.sha256(raw).hexdigest(),'dev':after.st_dev,
                 'inode':after.st_ino,'size':after.st_size,'mtime_ns':after.st_mtime_ns,
                 'ctime_ns':after.st_ctime_ns,'observed_wall_ns':time.time_ns()}


def call(name, path, interface, method, signature, args):
    return BUS.call_sync(name, path, interface, method, GLib.Variant(signature, args),
                         None, Gio.DBusCallFlags.NONE, 2000, None).unpack()


def bus(method, name):
    return call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                method, '(s)', (name,))[0]


def has_owner(name):
    return bus('NameHasOwner', name)


def process_identity(pid, exe):
    exe = exe.resolve(strict=True)
    metadata = exe.stat()
    if (Path(f'/proc/{pid}/exe').resolve(strict=True) != exe or metadata.st_uid != 0
            or metadata.st_mode & 0o022 or not stat.S_ISREG(metadata.st_mode)):
        raise RuntimeError('not the actual immutable installed executable')
    ticks = bounded(Path(f'/proc/{pid}/stat'), 8192, proc=True).decode().rsplit(')', 1)[1].split()[19]
    return {'pid': pid, 'uid': os.stat(f'/proc/{pid}').st_uid, 'start': ticks,
            'exe': str(exe), 'sha256': hashlib.sha256(bounded(exe)).hexdigest()}


def identity(name, exe, expected_pid=None):
    owner = bus('GetNameOwner', name)
    pid = bus('GetConnectionUnixProcessID', owner)
    uid = bus('GetConnectionUnixUser', owner)
    result = process_identity(pid, exe)
    if uid != 1000 or result['uid'] != uid or (expected_pid is not None and pid != expected_pid):
        raise RuntimeError('original ordinary-user bus process mismatch')
    if bus('GetNameOwner', name) != owner or bus('GetConnectionUnixProcessID', owner) != pid or bus('GetConnectionUnixUser', owner) != uid:
        raise RuntimeError('original bus owner changed during bounded identity observation')
    if process_identity(pid, exe) != result:
        raise RuntimeError('original process changed during bus owner observation')
    result['owner'] = owner
    return result


def guard(name, exe, original):
    if identity(name, exe, original['pid']) != original:
        raise RuntimeError('original service identity changed: '+name)


def wait(predicate, label, seconds=20):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        if predicate():
            return
        time.sleep(.05)
    raise RuntimeError(label)


def scene():
    return json.loads(bounded(STATE, 1024 * 1024))


def effective():
    return call(COLOR, '/org/gnome/SettingsDaemon/Color', 'org.freedesktop.DBus.Properties',
                'Get', '(ss)', (COLOR, 'Temperature'))[0]


def supported():
    return call(DISPLAY_CONFIG, '/org/gnome/Mutter/DisplayConfig', 'org.freedesktop.DBus.Properties',
                'Get', '(ss)', (DISPLAY_CONFIG, 'NightLightSupported'))[0]


def persisted():
    return {key: command(['gsettings', 'get', 'org.gnome.settings-daemon.plugins.color', key])
            for key in ['night-light-enabled', 'night-light-temperature',
                        'night-light-schedule-automatic', 'night-light-schedule-from',
                        'night-light-schedule-to']}


def tree(pid, label):
    desktop = pyatspi.Registry.getDesktop(0)
    if not 0 <= desktop.childCount <= 256:
        raise RuntimeError('accessibility application count exceeds bound')
    apps = [desktop.getChildAtIndex(i) for i in range(desktop.childCount)]
    apps = [app for app in apps if app is not None and app.get_process_id() == pid]
    if len(apps) > 1:
        raise RuntimeError('duplicate original accessibility application')
    nodes, showing = [], []
    def walk(node, depth):
        if depth > 32 or len(nodes) >= 4096 or not 0 <= node.childCount <= 256:
            raise RuntimeError('accessibility tree exceeds bounds')
        node.clear_cache()
        name = node.name or ''
        if len(name) > 1024:
            raise RuntimeError('accessibility name exceeds bound')
        state = node.getState()
        row = {'name': name, 'role': node.getRoleName(), 'showing': state.contains(pyatspi.STATE_SHOWING),
               'sensitive': state.contains(pyatspi.STATE_SENSITIVE), 'checked': state.contains(pyatspi.STATE_CHECKED)}
        nodes.append(row)
        if row['showing']:
            showing.append(node)
        for i in range(node.childCount):
            child = node.getChildAtIndex(i)
            if child is not None:
                walk(child, depth + 1)
    if apps:
        walk(apps[0], 0)
    (OUT / (label+'-a11y.json')).write_text(json.dumps(nodes, indent=2))
    return showing


def unique(pid, label, predicate):
    matches = [node for node in tree(pid, label) if predicate(node)]
    if len(matches) != 1:
        raise RuntimeError('required original SHOWING widget is not unique: '+label)
    return matches[0]


def action(node, label):
    if not node.getState().contains(pyatspi.STATE_SENSITIVE):
        raise RuntimeError('target is not sensitive: '+label)
    iface = node.queryAction()
    names = [iface.getName(i) for i in range(iface.nActions)]
    (OUT / (label+'-actions.json')).write_text(json.dumps(names))
    if len(names) != 1 or names[0] not in ('click', 'activate', 'toggle', 'press') or not iface.doAction(0):
        raise RuntimeError('actual widget has no verified single action: '+label)


def toggle(process, original, desired, label):
    guard(SETTINGS, CC, original)
    node = unique(process.pid, label, lambda n: n.name.replace('_', '') == 'Night Light'
                  and n.getRoleName() in ('switch', 'toggle button', 'check box'))
    if node.getState().contains(pyatspi.STATE_CHECKED) == desired:
        raise RuntimeError('no actual toggle transition to prove')
    action(node, label)
    wait(lambda: persisted()['night-light-enabled'] == str(desired).lower(), 'actual UI key did not persist')
    guard(SETTINGS, CC, original)


def manual(process, original, start, end, label):
    guard(SETTINGS, CC, original)
    if command(['gsettings', 'get', 'org.gnome.desktop.interface', 'clock-format']) != "'24h'":
        raise RuntimeError('prepared genuine spinner order requires actual default24h contract')
    if persisted()['night-light-schedule-automatic'] == 'true':
        selector = unique(process.pid, label+'-schedule', lambda n: n.getRoleName() == 'combo box')
        action(selector, label+'-schedule')
        choice = unique(process.pid, label+'-manual-choice', lambda n: n.name == 'Manual Schedule'
                        and n.getRoleName() in ('list item', 'menu item', 'radio button'))
        action(choice, label+'-manual-choice')
        wait(lambda: persisted()['night-light-schedule-automatic'] == 'false', 'actual manual selection did not persist')
    spins = [node for node in tree(process.pid, label+'-times') if node.getRoleName() == 'spin button']
    if len(spins) != 4:
        raise RuntimeError('actual primary From/To spinner structure is not four unique24h fields')
    for node, value in zip(spins, [start.hour, start.minute, end.hour, end.minute]):
        if not node.queryValue().setCurrentValue(value):
            raise RuntimeError('actual time spinner refused UI value')
    expected = [start.hour + start.minute / 60, end.hour + end.minute / 60]
    wait(lambda: all(abs(float(persisted()[key])-value) < 1e-6 for key,value in
                     zip(['night-light-schedule-from', 'night-light-schedule-to'], expected)),
         'actual From/To widget values did not persist')
    guard(SETTINGS, CC, original)


def portal_capture(label, daemon_id, cc_id, display_id, host, fallback=False):
    guard(COLOR,DAEMON,daemon_id)
    guard(DISPLAY_CONFIG,COMPOSITOR,display_id)
    raw_before = transform_summary(scene()['night_light'])
    raw_begin_wall = time.time_ns()
    target = OUT / (label+'-raw-capture.png')
    log = (OUT / (label+'-portal.log')).open('w')
    client = subprocess.Popen(['python3', '/repo/scripts/lib/roost-portal-screenshot-client.py', str(target), 'grant'], stdout=log, stderr=log)
    log.close()
    provider_id = None
    try:
        wait(lambda: has_owner('org.freedesktop.impl.portal.desktop.gnome'), 'genuine capture backend absent')
        provider_id = identity('org.freedesktop.impl.portal.desktop.gnome', Path('/usr/libexec/xdg-desktop-portal-gnome'))
        end = time.monotonic() + 20
        acted = False
        while not target.exists() and client.poll() is None and time.monotonic() < end:
            nodes = tree(provider_id['pid'], label+'-consent')
            buttons = [n for n in nodes if n.getRoleName() in ('push button','button') and n.name == 'Allow']
            if buttons:
                if acted or len(buttons) != 1:
                    raise RuntimeError('capture consent is not one original unique decision')
                guard('org.freedesktop.impl.portal.desktop.gnome', Path('/usr/libexec/xdg-desktop-portal-gnome'), provider_id)
                action(buttons[0], label+'-allow')
                acted = True
            time.sleep(.05)
        client.wait(timeout=5)
        if client.returncode != 0 or not target.exists():
            raise RuntimeError('genuine capture frontend failed; raw logs retained')
        guard('org.freedesktop.impl.portal.desktop.gnome', Path('/usr/libexec/xdg-desktop-portal-gnome'), provider_id)
        raw_end_wall = time.time_ns()
        guard(COLOR,DAEMON,daemon_id)
        guard(DISPLAY_CONFIG,COMPOSITOR,display_id)
        raw_after = transform_summary(scene()['night_light'])
        if raw_before != raw_after:
            raise RuntimeError('original transform changed while ordinary scene capture was pending')
        row=display_receipt(label, daemon_id, cc_id, display_id, host, fallback)
        if row['transform_before'] != raw_after:
            raise RuntimeError('submitted display transform changed between ordinary capture and scrot')
        row['raw_capture_wall_ns']=[raw_begin_wall,raw_end_wall]
        row['raw_capture_transform_before']=raw_before
        row['raw_capture_transform_after']=raw_after
        (OUT/'night-light-journey.json').write_text(json.dumps(records,indent=2))
        return target
    finally:
        if client.poll() is None:
            client.terminate()
            client.wait(timeout=5)


def pixels(path):
    data = bounded(path, 32 * 1024 * 1024)
    loader = GdkPixbuf.PixbufLoader.new_with_type('png')
    loader.write(data)
    loader.close()
    image = loader.get_pixbuf()
    if (image.get_width(), image.get_height()) != (1280,800):
        raise RuntimeError('actual image dimensions disagree with original output')
    values, stride, channels = bytes(image.get_pixels()), image.get_rowstride(), image.get_n_channels()
    if channels not in (3,4):
        raise RuntimeError('unsupported actual PNG channel layout')
    def at(x,y):
        offset = y*stride+x*channels
        return list(values[offset:offset+3])
    return at


def pixel_proof(label, capture, temperature, baseline=None, probes=None):
    static_guard()
    raw = pixels(capture)
    visible = pixels(OUT / (label+'-display.png'))
    scales = reference(temperature)
    if probes is None:
        probes = STATIC_PROBES
    if len(probes) != 5:
        raise RuntimeError('five independently meaningful interior probes unavailable')
    receipt = []
    changed = False
    for index,(x,y) in enumerate(probes):
        source, actual = raw(x,y), visible(x,y)
        if probes != STATIC_PROBES or any(abs(a-b)>1 for a,b in zip(source,STATIC_COLORS[index])):
            raise RuntimeError('owned static source is occluded, relocated or changed at '+str((x,y)))
        expected = [round(value*scale) for value,scale in zip(source,scales)]
        if any(abs(a-b)>1 for a,b in zip(actual,expected)):
            raise RuntimeError('actual display color differs from independent libcolord reference at '+str((x,y)))
        if baseline is not None and any(abs(a-b)>1 for a,b in zip(source,baseline[(x,y)])):
            raise RuntimeError('ordinary capture changed color under display-only warmth at '+str((x,y)))
        changed |= any(abs(a-b)>2 for a,b in zip(actual,source))
        receipt.append({'point':[x,y], 'raw':source, 'visible':actual, 'independent_expected':expected})
    if temperature != 6500 and not changed:
        raise RuntimeError('no measurable genuine output transform')
    (OUT / (label+'-pixels.json')).write_text(json.dumps({'temperature':temperature,'scales':scales,'probes':receipt,'tolerance':1,'original_static_client':STATIC_ID,'declared_source_rgb':STATIC_COLORS},indent=2))
    return probes, {(x,y):raw(x,y) for x,y in probes}


def static_guard():
    guard('org.roost.NightLight.StaticProbes',Path('/usr/bin/python3'),STATIC_ID)
    arguments=bounded(Path(f"/proc/{STATIC_ID['pid']}/cmdline"),8192,proc=True).split(b'\0')
    if arguments not in ([b'python3',b'/repo/scripts/lib/roost-night-light-static-client.py',b''],
                         [b'/usr/bin/python3',b'/repo/scripts/lib/roost-night-light-static-client.py',b'']):
        raise RuntimeError('original static process command changed')
    layers=[row for row in scene()['layers'] if row['namespace']=='roost-night-light-static-probes']
    if len(layers)!=1 or layers[0]['rect'] != [0,0,120,800]:
        raise RuntimeError('original static source layer geometry changed: '+json.dumps(layers)[:512])


def host_identity():
    windows = command(['xdotool', 'search', '--onlyvisible', '--name', '^Smithay']).split()
    matches = [wid for wid in windows if int(command(['xdotool', 'getwindowpid', wid])) == COMPOSITOR_PID]
    if len(matches) != 1:
        raise RuntimeError('original candidate Smithay host is not unique')
    wid = matches[0]
    identity = process_identity(COMPOSITOR_PID, COMPOSITOR)
    return {'xid': wid, 'process': identity, 'geometry': command(['xdotool', 'getwindowgeometry', '--shell', wid])}


def host_guard(original):
    if host_identity() != original:
        raise RuntimeError('original compositor host identity/geometry changed')


def capture_baseline_receipt(fd_receipt, capture_started):
    if type(capture_started) is not int or not 0 < capture_started <= fd_receipt['observed_wall_ns']:
        raise RuntimeError('actual capture clock interval reversed')
    receipt=dict(fd_receipt)
    receipt['observed_wall_ns']=capture_started
    return receipt


def display_receipt(label, daemon_id, cc_id, display_id, host, fallback=False):
    guard(COLOR, DAEMON, daemon_id)
    guard(SETTINGS, CC, cc_id)
    guard(DISPLAY_CONFIG, COMPOSITOR, display_id)
    host_guard(host)
    static_guard()
    before_transform = transform_summary(scene()['night_light'])
    before = time.time_ns()
    command(['scrot', '-o', str(OUT / (label+'-display.png'))])
    after = time.time_ns()
    raw_state, fd_receipt = original_state_receipt()
    state_receipt = capture_baseline_receipt(fd_receipt,before)
    (OUT / (label+'-state.json')).write_bytes(raw_state)
    current = json.loads(raw_state)
    after_transform = transform_summary(current['night_light'])
    if before_transform != after_transform:
        raise RuntimeError('original submitted transform changed while capturing actual display pixels')
    guard(COLOR,DAEMON,daemon_id)
    guard(DISPLAY_CONFIG,COMPOSITOR,display_id)
    static_guard()
    submitted = current['night_light']['outputs'][0]['last_submitted_transform']
    if (submitted is None or submitted[:2] != [current['night_light']['owner_epoch'], current['night_light']['generation']]
            or any(abs(a-b)>0.00001 for a,b in zip(submitted[3], [1.,1.,1.] if fallback else current['night_light']['service_rgb_scales']))):
        raise RuntimeError('actual last submitted transform is stale versus retained service snapshot')
    row = {'label': label, 'capture_wall_ns': [before, after], 'effective_temperature': effective(),
           'candidate': current['night_light'], 'keys': persisted(), 'daemon': daemon_id,
           'settings': cc_id, 'display_owner': display_id, 'host': host,
           'state_receipt': state_receipt,'fd_read_wall_ns':fd_receipt['observed_wall_ns'],'transform_before':before_transform,'transform_after':after_transform}

    if row['candidate']['supported'] == fallback or row['candidate']['temperature'] != row['effective_temperature']:
        raise RuntimeError('effective daemon, candidate and capability disagree')
    records.append(row)
    (OUT / 'night-light-journey.json').write_text(json.dumps(records, indent=2))
    guard(COLOR, DAEMON, daemon_id)
    guard(SETTINGS, CC, cc_id)
    host_guard(host)
    return row


def transform_summary(value):
    if not 1 <= len(value['outputs']) <= 8:
        raise RuntimeError('actual admitted output count bound')
    return {key:value[key] for key in ('owner_epoch','generation','temperature','service_rgb_scales')} | {
        'outputs':[{key:row[key] for key in ('name','physical_size','location','scale','last_submitted_transform')}
                   for row in value['outputs']]}


def fault_negative(daemon_id, cc_id, display_id, host, baseline, probes):
    # Independent warm pixels have already passed. Root admission must consume
    # the same original semantic transform within a bounded capture interval.
    # Independent FD/hash receipts remain distinct when counters advance.
    original = process_identity(COMPOSITOR_PID, COMPOSITOR)
    before = scene()['night_light']
    if not before['supported'] or before['temperature'] != 3700:
        raise RuntimeError('original feature fixture is not genuinely warm')
    # Admission age starts at the actual display capture, before scrot and
    # later library/probe/identity checks. Preserve the later FD observation
    # timestamp separately in the original journey receipt, never reset age.
    baseline_receipt = dict(records[-1]['state_receipt'])
    if baseline_receipt['observed_wall_ns'] != records[-1]['capture_wall_ns'][0]:
        raise RuntimeError('baseline timestamp is not the actual retained capture start')
    request = {'pid': COMPOSITOR_PID, 'start': int(original['start']), 'exe_sha256': original['sha256'],
               'baseline_receipt': baseline_receipt,
               'baseline_transform': transform_summary(records[-1]['candidate'])}
    with (OUT/'fault-request.json').open('x') as stream:
        json.dump(request,stream)
    wait(lambda:(OUT/'fault-controller-result.json').exists(),'root fault controller missing',20)
    controller = json.loads(bounded(OUT/'fault-controller-result.json',16384))
    if 'failure_type' in controller:
        raise RuntimeError('root controller rejected original warm receipt: '+controller['failure_type'])
    if controller['state_receipt']['transform'] != request['baseline_transform']:
        raise RuntimeError('root controller admitted a different original transform')
    wait(lambda:scene().get('night_light_fixture') == [1,1] and not supported()
         and not scene()['night_light']['supported'],'optional failure did not neutralize capability',20)
    captured = portal_capture('controlled-final-pass-neutral',daemon_id,cc_id,display_id,host,True)
    pixel_proof('controlled-final-pass-neutral',captured,6500,baseline,probes)
    after = scene()['night_light']
    expected_counters = [row['stage_counters'] for row in after['outputs']]
    if len(after['outputs']) != 1 or expected_counters[0][1] != before['outputs'][0]['stage_counters'][1]+1:
        raise RuntimeError('expected exactly one controlled stage failure')
    observations = []
    for _ in range(3):
        # Genuine subsequent portal UI/client paints exercise the latched path.
        path=portal_capture('latched-neutral-'+str(len(observations)),daemon_id,cc_id,display_id,host,True)
        pixel_proof('latched-neutral-'+str(len(observations)),path,6500,baseline,probes)
        current=scene()
        guard(DISPLAY_CONFIG,COMPOSITOR,display_id)
        if current['night_light_fixture'] != [1,1] or supported() or current['night_light']['supported']:
            raise RuntimeError('controlled failure replayed or capability recovered without context change')
        if [row['stage_counters'] for row in current['night_light']['outputs']] != expected_counters:
            raise RuntimeError('latched stage allocated or retried on subsequent client paint')
        observations.append({'wall_ns':time.time_ns(),'candidate':current['night_light'],
                             'fixture_counts':current['night_light_fixture']})
    (OUT/'controlled-failure-proof.json').write_text(json.dumps({'controller':controller,
        'before':before,'after':after,'subsequent_observations':observations,
        'injected_driver_failure':False,'normal_binary_qualified':False,
        'same_uid_snapshot_attribution':'isolated fixture assumption'},indent=2))


# Actual stock library, not a copied approximation or generated fixture.
class RGB(ctypes.Structure):
    _fields_ = [('red', ctypes.c_double), ('green', ctypes.c_double), ('blue', ctypes.c_double)]


library_path = Path('/usr/lib64/libcolord.so.2').resolve(strict=True)
library = ctypes.CDLL(str(library_path))
library.cd_color_get_blackbody_rgb_full.argtypes = [ctypes.c_double, ctypes.POINTER(RGB), ctypes.c_uint]
library.cd_color_get_blackbody_rgb_full.restype = ctypes.c_int


def reference(temperature):
    value = RGB()
    if not 1000 <= temperature <= 10000 or not library.cd_color_get_blackbody_rgb_full(temperature, ctypes.byref(value), 1):
        raise RuntimeError('genuine libcolord refused temperature')
    return [value.red, value.green, value.blue]


settings = None
restarted_daemon = None
try:
    if os.getuid() != 1000:
        raise RuntimeError('journey must run as original ordinary UID1000')
    daemon_id = identity(COLOR, DAEMON, int(os.environ['ROOST_NIGHT_LIGHT_DAEMON_PID']))
    display_id = identity(DISPLAY_CONFIG, COMPOSITOR, COMPOSITOR_PID)
    host = host_identity()
    wait(lambda:has_owner('org.roost.NightLight.StaticProbes'),'actual static pixel source did not acquire name')
    STATIC_ID = identity('org.roost.NightLight.StaticProbes',Path('/usr/bin/python3'),int(os.environ['ROOST_NIGHT_LIGHT_STATIC_PID']))
    wait(lambda:any(row['namespace']=='roost-night-light-static-probes' for row in scene()['layers']),
         'actual static GTK layer source never mapped')
    # The first mapped commit can precede the compositor configure; only the
    # settled original geometry proves the genuine static source is in place.
    wait(lambda:[row['rect'] for row in scene()['layers'] if row['namespace']=='roost-night-light-static-probes']==[[0,0,120,800]],
         'actual static GTK layer source never settled to original geometry')
    static_guard()
    (OUT/'static-source-identity.json').write_text(json.dumps({'process':STATIC_ID,
        'script_sha256':hashlib.sha256(bounded(Path('/repo/scripts/lib/roost-night-light-static-client.py'))).hexdigest(),
        'geometry':[0,0,120,800],'colors':STATIC_COLORS,'probes':STATIC_PROBES},indent=2))
    wait(supported, 'actual initialized NightLightSupported never became true')
    if reference(6500) != [1.0, 1.0, 1.0]:
        raise RuntimeError('installed Planckian6500 is not the primary neutral contract')
    (OUT / 'colord-reference.json').write_text(json.dumps({'library': str(library_path),
        'sha256': hashlib.sha256(bounded(library_path)).hexdigest(),
        'owner': command(['rpm', '-qf', str(library_path)]), 'neutral': reference(6500)}, indent=2))
    log = (OUT / 'settings.log').open('w')
    settings = subprocess.Popen([str(CC), 'display'], stdout=log, stderr=log)
    log.close()
    wait(lambda: has_owner(SETTINGS), 'stock Settings did not acquire original bus name')
    cc_id = identity(SETTINGS, CC, settings.pid)
    wait(lambda: scene().get('focused_app_id') == SETTINGS, 'stock Settings lacks actual compositor focus')
    wait(lambda: any(n.name.replace('_', '') == 'Night Light' for n in tree(settings.pid, 'display-panel')),
         'actual Night Light navigation did not appear')
    navigation = unique(settings.pid, 'night-light-navigation', lambda n: n.name.replace('_', '') == 'Night Light' and n.getRoleName() not in ('label','static','text'))
    action(navigation, 'night-light-navigation')
    wait(lambda: any(n.getRoleName() == 'slider' for n in tree(settings.pid, 'night-light-page')),
         'actual temperature slider did not appear')
    if persisted()['night-light-enabled'] != 'false':
        raise RuntimeError('fresh actual schema does not start disabled')
    neutral_capture = portal_capture('initial-neutral', daemon_id, cc_id, display_id, host)
    probes, baseline = pixel_proof('initial-neutral', neutral_capture, 6500)
    toggle(settings, cc_id, True, 'enable')
    now = datetime.datetime.now().astimezone()
    manual(settings, cc_id, now-datetime.timedelta(hours=2), now+datetime.timedelta(hours=3), 'active-manual')
    slider = unique(settings.pid, 'temperature', lambda n: n.getRoleName() == 'slider')
    if not slider.queryValue().setCurrentValue(3700):
        raise RuntimeError('actual Settings temperature slider refused value input')
    wait(lambda: persisted()['night-light-temperature'] == 'uint32 3700', 'slider did not write shared persistent key')
    wait(lambda: effective() == 3700 and scene()['night_light']['temperature'] == 3700,
         'genuine active schedule/slider did not reach candidate output', 20)
    warm_capture = portal_capture('actual-slider-warm', daemon_id, cc_id, display_id, host)
    pixel_proof('actual-slider-warm', warm_capture, 3700, baseline, probes)
    if FIXTURE_NEGATIVE:
        fault_negative(daemon_id,cc_id,display_id,host,baseline,probes)
        (OUT/'stage-scope.json').write_text(json.dumps({'actual_completed':True,'negative_fixture_only':True,
            'normal_binary_qualified':False,'physical_driver_failure_qualified':False,'full339_qualified':False},indent=2))
        raise SystemExit(0)
    # Genuine service API temporary-disable; actual Settings Restart Filter
    # clears it. This is not claimed as a GUI-created temporary disable.
    guard(COLOR,DAEMON,daemon_id)
    call(COLOR,'/org/gnome/SettingsDaemon/Color','org.freedesktop.DBus.Properties','Set','(ssv)',
         (COLOR,'DisabledUntilTomorrow',GLib.Variant('b',True)))
    wait(lambda: effective()==6500 and scene()['night_light']['temperature']==6500,
         'real temporary disable did not restore neutral',20)
    if persisted()['night-light-enabled']!='true' or not supported():
        raise RuntimeError('temporary disable incorrectly changed enabled/capability')
    disabled_capture=portal_capture('real-temporary-disable',daemon_id,cc_id,display_id,host)
    pixel_proof('real-temporary-disable',disabled_capture,6500,baseline,probes)
    restart=unique(settings.pid,'restart-filter',lambda n:n.name=='Restart Filter' and n.getRoleName() in ('push button','button'))
    action(restart,'restart-filter')
    wait(lambda: not call(COLOR,'/org/gnome/SettingsDaemon/Color','org.freedesktop.DBus.Properties','Get','(ss)',
                         (COLOR,'DisabledUntilTomorrow'))[0] and effective()==3700 and scene()['night_light']['temperature']==3700,
         'actual Restart Filter did not restore effective warm schedule',20)
    restarted_filter_capture=portal_capture('actual-restart-filter',daemon_id,cc_id,display_id,host)
    pixel_proof('actual-restart-filter',restarted_filter_capture,3700,baseline,probes)
    # Genuine daemon loss must reset visible output and truthful capability.
    guard(COLOR, DAEMON, daemon_id)
    os.kill(daemon_id['pid'], signal.SIGTERM)
    wait(lambda: not has_owner(COLOR) and not supported() and not scene()['night_light']['service_supported'],
         'lost original daemon left capability or service warmth enabled')
    command(['scrot','-o',str(OUT/'owner-loss-display.png')])
    lost = pixels(OUT/'owner-loss-display.png')
    if any(any(abs(a-b)>1 for a,b in zip(lost(*point),baseline[point])) for point in probes):
        raise RuntimeError('owner loss did not restore actual neutral output')
    # Negative only: reserve the service name from this unrelated Python
    # process, without implementing or substituting any positive policy API.
    request=call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus',
                 'RequestName','(su)',(COLOR,4))[0]
    if request!=1 or bus('GetConnectionUnixProcessID',bus('GetNameOwner',COLOR))!=os.getpid():
        raise RuntimeError('forged-name negative did not acquire its own original unrelated process')
    forged=[]
    try:
        end=time.monotonic()+3
        while time.monotonic()<end:
            current=scene()['night_light']
            forged.append({'wall_ns':time.time_ns(),'candidate':current,'display_supported':supported()})
            if current['service_supported'] or supported():
                raise RuntimeError('unrelated process with genuine service name was trusted')
            time.sleep(.1)
        if len(forged)<20:
            raise RuntimeError('insufficient forged-name negative observations')
    finally:
        (OUT/'forged-service-negative.json').write_text(json.dumps(forged,indent=2))
        call('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus','ReleaseName','(s)',(COLOR,))
    wait(lambda:not has_owner(COLOR),'negative forged bus owner did not release')
    original_epoch = records[-1]['candidate']['owner_epoch']
    daemon_log = (OUT/'daemon-restarted.log').open('w')
    restarted_daemon = subprocess.Popen([str(DAEMON)],stdout=daemon_log,stderr=daemon_log)
    daemon_log.close()
    wait(lambda: has_owner(COLOR), 'genuine daemon did not reacquire owner after restart')
    restarted_id = identity(COLOR,DAEMON,restarted_daemon.pid)
    if restarted_id['owner'] == daemon_id['owner'] or restarted_id['start'] == daemon_id['start']:
        raise RuntimeError('genuine daemon owner did not change')
    daemon_id = restarted_id
    wait(lambda: supported() and effective()==3700 and scene()['night_light']['temperature']==3700,
         'new genuine owner did not restore persisted active schedule',20)
    if scene()['night_light']['owner_epoch'] <= original_epoch:
        raise RuntimeError('new genuine daemon did not advance owner epoch')
    recovered_capture = portal_capture('owner-recovery-warm',daemon_id,cc_id,display_id,host)
    pixel_proof('owner-recovery-warm',recovered_capture,3700,baseline,probes)
    # Real Settings restart, not reopening an existing unique app instance.
    guard(SETTINGS,CC,cc_id)
    settings.terminate()
    settings.wait(timeout=5)
    wait(lambda: not has_owner(SETTINGS),'original Settings retained owner after close')
    log = (OUT/'settings-restarted.log').open('w')
    settings = subprocess.Popen([str(CC),'display'],stdout=log,stderr=log)
    log.close()
    wait(lambda: has_owner(SETTINGS),'Settings did not genuinely restart')
    replacement = identity(SETTINGS,CC,settings.pid)
    if replacement['owner']==cc_id['owner'] or replacement['pid']==cc_id['pid']:
        raise RuntimeError('Settings restart identity unchanged')
    cc_id = replacement
    wait(lambda: any(n.name.replace('_','')=='Night Light' for n in tree(settings.pid,'restarted-panel')), 'Night Light row absent after restart')
    navigation = unique(settings.pid,'restarted-navigation',lambda n:n.name.replace('_','')=='Night Light' and n.getRoleName() not in ('label','static','text'))
    action(navigation,'restarted-navigation')
    wait(lambda: any(n.getRoleName()=='slider' for n in tree(settings.pid,'restarted-page')), 'restarted actual temperature slider absent')
    if persisted()['night-light-enabled']!='true' or persisted()['night-light-temperature']!='uint32 3700':
        raise RuntimeError('actual persisted controls did not survive Settings restart')
    (OUT/'persisted-settings-keyfile.txt').write_bytes(bounded(OUT/'config/glib-2.0/settings/keyfile',64*1024))
    # Real natural wallclock end: UI writes a boundary ~1-2min ahead. GSD owns
    # its 60s calendar poll and5s smoothing, never a substituted test clock.
    now = datetime.datetime.now().astimezone()
    boundary = (now+datetime.timedelta(minutes=2)).replace(second=0,microsecond=0)
    manual(settings,cc_id,now-datetime.timedelta(hours=2),boundary,'natural-manual-end')
    observations=[]
    deadline=time.monotonic()+240
    while time.monotonic()<deadline:
        guard(COLOR,DAEMON,daemon_id)
        timestamp=time.time_ns()
        temperature=effective()
        observations.append({'wall_ns':timestamp,'temperature':temperature,'candidate':scene()['night_light']})
        if datetime.datetime.now().astimezone()>=boundary and temperature==6500 and scene()['night_light']['temperature']==6500:
            break
        time.sleep(.2)
    else:
        raise RuntimeError('real natural schedule did not become neutral after boundary')
    if not any(row['temperature']<6500 for row in observations):
        raise RuntimeError('natural schedule never produced an active pre-boundary observation')
    (OUT/'natural-schedule.json').write_text(json.dumps({'boundary':boundary.isoformat(),'observations':observations},indent=2))
    natural_capture=portal_capture('natural-schedule-neutral',daemon_id,cc_id,display_id,host)
    pixel_proof('natural-schedule-neutral',natural_capture,6500,baseline,probes)
    # Outside schedule, one actual slider value change must cause the genuine
    # CC five-second Preview; keep natural wallclock/provider/submission trace.
    now=datetime.datetime.now().astimezone()
    manual(settings,cc_id,now+datetime.timedelta(hours=3),now+datetime.timedelta(hours=5),'outside-schedule')
    wait(lambda: effective()==6500,'outside real schedule did not remain neutral',20)
    slider=unique(settings.pid,'outside-preview-slider',lambda n:n.getRoleName()=='slider')
    if not slider.queryValue().setCurrentValue(2700):
        raise RuntimeError('actual outside-schedule slider refused input')
    preview=[]
    deadline=time.monotonic()+20
    saw_warm=False
    while time.monotonic()<deadline:
        guard(COLOR,DAEMON,daemon_id)
        current=scene()['night_light']
        value=effective()
        preview.append({'wall_ns':time.time_ns(),'temperature':value,'candidate':current})
        saw_warm |= value<6200 and current['temperature']<6200
        if saw_warm and value==6500 and current['temperature']==6500:
            break
        time.sleep(.05)
    if not saw_warm or preview[-1]['temperature']!=6500:
        raise RuntimeError('actual genuine preview did not warm then expire to original outside-schedule neutral')
    if persisted()['night-light-enabled']!='true' or persisted()['night-light-temperature']!='uint32 2700':
        raise RuntimeError('preview incorrectly changed enabled or lost actual slider preference')
    (OUT/'outside-schedule-preview.json').write_text(json.dumps(preview,indent=2))
    preview_expired_capture=portal_capture('outside-preview-expired',daemon_id,cc_id,display_id,host)
    pixel_proof('outside-preview-expired',preview_expired_capture,6500,baseline,probes)
    (OUT/'stage-scope.json').write_text(json.dumps({'actual_completed':True,'full339_qualified':False,
        'unqualified':['preview active-phase pixel timing','automatic geolocation',
                       'full compositor restart','window/area/monitor stream pixels','native multi-output',
                       'hardware recovery/final image','GL failure negative separate fixture']},indent=2))

except Exception as error:
    (OUT/'failure.json').write_text(json.dumps({'type':type(error).__name__,'message':str(error)[:4096],'wall_ns':time.time_ns()},indent=2))
    raise
finally:
    if 'observations' in globals():
        (OUT/'natural-schedule-progress.json').write_text(json.dumps(observations,indent=2))
    if 'preview' in globals():
        (OUT/'outside-preview-progress.json').write_text(json.dumps(preview,indent=2))
    if settings is not None and settings.poll() is None:
        settings.terminate()
        settings.wait(timeout=5)

    if restarted_daemon is not None and restarted_daemon.poll() is None:
        restarted_daemon.terminate()
        restarted_daemon.wait(timeout=5)
