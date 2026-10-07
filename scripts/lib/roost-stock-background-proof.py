#!/usr/bin/python3
"""Real stock background chooser, retained pixels and session restart proof."""
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
import sys
import struct
import zlib
import xml.etree.ElementTree as ET
import gi
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf
PHASE = sys.argv[1]


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


FIXTURE = OUT / 'data/gnome-background-properties'
PICTURE = OUT / 'static-bands.png'
COLORS = [(240,0,0),(0,0,240),(240,240,0),(120,0,160)]
POINTS = [(10,780),(300,780),(640,400),(1270,780),(10,34)]
CASES = ['centered','scaled','stretched','zoom','spanned','wallpaper','horizontal','vertical']


def write_picture():
    def chunk(kind, data):
        return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    rows = b''.join(b'\x00'+b''.join(bytes(COLORS[x//16]) for x in range(64)) for _ in range(32))
    PICTURE.write_bytes(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',64,32,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(rows))+chunk(b'IEND',b''))

def fixture():
    # Genuine wallpaper-list metadata consumed by the unmodified stock app.
    FIXTURE.mkdir(parents=True)
    write_picture()
    root=ET.Element('wallpapers')
    for case in CASES:
        item=ET.SubElement(root,'wallpaper',{'deleted':'false'})
        for tag,value in [('name','Roost '+case+' proof'),('filename',str(PICTURE)),
                          ('options','none' if case in ('horizontal','vertical') else case),
                          ('shade_type',case if case in ('horizontal','vertical') else 'solid'),
                          ('pcolor','#00c800'),('scolor','#0000c8')]:
            ET.SubElement(item,tag).text=value
    ET.ElementTree(root).write(FIXTURE/'roost-static.xml',encoding='utf-8',xml_declaration=True)
    (OUT/'fixture-identity.json').write_text(json.dumps({'picture':str(PICTURE),'bytes':PICTURE.stat().st_size,'sha256':hashlib.sha256(PICTURE.read_bytes()).hexdigest()},indent=2))


def controls(process, original, label):
    guard(process,original)
    nodes, found=[],[]
    def walk(node,depth):
        if depth>32 or len(nodes)>=4096: raise RuntimeError('bounded accessible tree exceeded')
        node.clear_cache()
        state=node.getState()
        showing=state.contains(pyatspi.STATE_SHOWING)
        record={'name':node.name or '', 'role':node.getRoleName(),'showing':showing,
                'checked':state.contains(pyatspi.STATE_CHECKED),'depth':depth}
        nodes.append(record)
        if state.contains(pyatspi.STATE_VISIBLE) and record['role']=='toggle button': found.append(node)
        for index in range(node.childCount):
            child=node.getChildAtIndex(index)
            if child is not None: walk(child,depth+1)
    desktop=pyatspi.Registry.getDesktop(0)
    for index in range(desktop.childCount):
        app=desktop.getChildAtIndex(index)
        if app is not None and app.get_process_id()==original['pid']: walk(app,0)
    (OUT/f'{label}-a11y.json').write_text(json.dumps(nodes,indent=2))
    return found


def activate_chooser(control, process, original, label):
    # GNOME's chooser is GtkFlowBoxChild with an overridden accessible role.
    # Its generic Action indices enumerate muxer actions, not child::activate.
    # Retain that real inventory and activate the documented Enter keybinding.
    action = control.queryAction()
    actions = [action.getName(i) for i in range(action.nActions)]
    control.clear_cache()
    if not control.getState().contains(pyatspi.STATE_SHOWING):
        raise RuntimeError('actual chooser target is not showing')
    if not control.queryComponent().grabFocus():
        raise RuntimeError('actual chooser target refused focus')
    def focused():
        guard(process, original)
        control.clear_cache()
        return (control.getState().contains(pyatspi.STATE_FOCUSED)
                and scene().get('focused_app_id') == NAME)
    wait_for(focused, 'actual chooser target did not acquire focus')
    hosts = subprocess.check_output(['xdotool', 'search', '--onlyvisible', '--name', '^Smithay'], text=True).split()
    if len(hosts) != 1:
        raise RuntimeError('expected exactly one actual Smithay keyboard host')
    host = hosts[0]
    pid = int(subprocess.check_output(['xdotool', 'getwindowpid', host], text=True))
    executable = Path(f'/proc/{pid}/exe').resolve(strict=True)
    start = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
    actual = {'pid': pid, 'executable': str(executable), 'start_ticks': start,
              'sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}
    if actual != ORIGINAL_HOST:
        raise RuntimeError('keyboard host identity changed from the original candidate compositor')
    subprocess.run(['xdotool', 'windowfocus', '--sync', host], check=True, timeout=5)
    actual_focus = int(subprocess.check_output(['xdotool', 'getwindowfocus'], text=True))
    if actual_focus != int(host) or not focused():
        raise RuntimeError('actual host/chooser focus changed before the key')
    (OUT/f'{label}-keyboard-host.json').write_text(json.dumps({
        'host': actual, 'window': int(host), 'actual_x_focus': actual_focus,
        'target': control.name, 'actions': actions, 'target_focused': True,
        'scene_before_key': scene(), 'settings_identity': original}, indent=2))
    # One genuine event, without SendEvent, direct setters, retries or replay.
    subprocess.run(['xdotool', 'key', 'Return'], check=True)
    guard(process, original)
    if Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19] != start:
        raise RuntimeError('keyboard host changed during the key')


def select(case):
    log=(OUT/f'{PHASE}-{case}-settings.log').open('w')
    process=subprocess.Popen([str(EXE),'background'],stdout=log,stderr=log)
    original=None
    try:
        wait_for(lambda: bus_call('NameHasOwner','(s)',(NAME,)), 'actual Settings bus missing')
        original=identity(process)
        wait_for(lambda: scene().get('focused_app_id')==NAME, 'actual Settings did not receive compositor focus')
        (OUT/f'{PHASE}-{case}-identity.json').write_text(json.dumps(original,indent=2))
        target='Roost '+case+' proof'
        def find():
            matching=[c for c in controls(process,original,PHASE+'-'+case) if c.name==target]
            if len(matching)>1: raise RuntimeError('nonunique stock chooser item')
            return matching[0] if matching else None
        wait_for(lambda: find() is not None,'stock chooser fixture item missing',30)
        # Stock Background appears below Style/Accent: scroll the actual item,
        # then invoke its real accessibility action once, never set a key.
        control=find()
        control.clear_cache()
        if not control.getState().contains(pyatspi.STATE_SHOWING):
            control.queryComponent().scrollTo(pyatspi.SCROLL_ANYWHERE)
            drain()
        control.clear_cache()
        if not control.getState().contains(pyatspi.STATE_SHOWING): raise RuntimeError('actual chooser item not showing after scroll')
        activate_chooser(control, process, original, PHASE+'-'+case)
        def saved():
            observed={}
            for schema in ['org.gnome.desktop.background','org.gnome.desktop.screensaver']:
                values={key:subprocess.check_output(['gsettings','get',schema,key],text=True).strip()
                        for key in ['picture-uri','picture-options','color-shading-type','primary-color','secondary-color']}
                observed[schema]=values
            (OUT/f'{PHASE}-{case}-keys.json').write_text(json.dumps(observed,indent=2))
            expected_option='none' if case in ('horizontal','vertical') else case
            return all(values['picture-options']==repr(expected_option)
                       and values['color-shading-type']==repr(case if case in ('horizontal','vertical') else 'solid')
                       and values['primary-color']==repr('#00c800') and values['secondary-color']==repr('#0000c8')
                       and values['picture-uri']==repr(PICTURE.as_uri()) for values in observed.values())
        wait_for(saved,'actual chooser did not persist full desktop+lock metadata')
        guard(process,original)
        selected=find()
        if not selected.getState().contains(pyatspi.STATE_CHECKED): raise RuntimeError('actual selected chooser item unchecked')
        subprocess.run(['scrot','-o',str(OUT/f'{PHASE}-{case}-settings.png')],check=True)
    finally:
        process.terminate();process.wait(timeout=10);log.close()
    wait_for(lambda: not bus_call('NameHasOwner','(s)',(NAME,)), 'actual Settings did not exit')
    wait_for(lambda: not scene()['windows'], 'background proof desktop still contains applications')
    return original


def expected(case,x,y):
    if case=='horizontal':
        t=min(1,max(0,2*(x+.5)/1280-.5))
        return (0,round(200*(1-t)),round(200*t))
    if case=='vertical':
        t=min(1,max(0,2*(y+.5)/800-.5))
        return (0,round(200*(1-t)),round(200*t))
    if case=='centered':
        if not(608<=x<672 and 384<=y<416): return(0,200,0)
        sx=x-608
    elif case=='scaled':
        if not 80<=y<720:return(0,200,0)
        sx=(x+.5)/20-.5
    elif case in ('stretched','spanned'): sx=(x+.5)/20-.5
    elif case=='zoom':sx=(x+160+.5)/25-.5
    elif case=='wallpaper':sx=(x-608)%64
    else:raise RuntimeError('unexpected case')
    # Probe pixels are well inside known solid fixture bands, away from edges.
    return COLORS[max(0,min(63,int(sx)))//16]


def pixels(case):
    path=OUT/f'{PHASE}-{case}-desktop.png'
    def correct():
        subprocess.run(['scrot','-o',str(path)],check=True)
        pixbuf=GdkPixbuf.Pixbuf.new_from_file(str(path))
        if pixbuf.get_width()!=1280 or pixbuf.get_height()!=800: raise RuntimeError('wrong actual screenshot dimensions')
        data=pixbuf.get_pixels(); stride=pixbuf.get_rowstride(); channels=pixbuf.get_n_channels()
        observed=[]
        for x,y in POINTS:
            offset=y*stride+x*channels
            actual=tuple(data[offset:offset+3]);want=expected(case,x,y)
            observed.append({'point':[x,y],'actual':actual,'expected':want})
        (OUT/f'{PHASE}-{case}-pixels.json').write_text(json.dumps(observed,indent=2))
        return all(max(abs(a-b) for a,b in zip(p['actual'],p['expected']))<=1 for p in observed)
    wait_for(correct,'actual desktop pixels did not match independently specified placement/gradient',30)
    metadata=json.loads((OUT/'runtime/roost-wallpaper').read_text().splitlines()[4])
    if metadata['version']!=1:raise RuntimeError('new publisher omitted versioned metadata')
    (OUT/f'{PHASE}-{case}-metadata.json').write_text(json.dumps(metadata,indent=2))
    (OUT/f'{PHASE}-{case}-scene.json').write_text(json.dumps(scene(),indent=2))


def session_identity():
    owner=bus_call('GetNameOwner','(s)',('org.gnome.Shell',))
    pid=bus_call('GetConnectionUnixProcessID','(s)',(owner,))
    exe=Path(f'/proc/{pid}/exe').resolve()
    if exe!=Path('/candidate/usr/bin/roost-shell-gtk').resolve():raise RuntimeError('not actual candidate GTK shell')
    stat_text=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
    return {'owner':owner,'pid':pid,'start_time':stat_text[19],'executable':str(exe),'sha256':hashlib.sha256(exe.read_bytes()).hexdigest(),'compositor_pid':int(stat_text[1])}


wait_for(lambda: bus_call('NameHasOwner','(s)',('org.gnome.Shell',)),'GTK shell did not start',30)
current_session=session_identity()
compositor_pid=current_session['compositor_pid']
compositor_exe=Path(f'/proc/{compositor_pid}/exe').resolve(strict=True)
if compositor_exe != Path('/candidate/usr/bin/roost-compositor').resolve(strict=True):
    raise RuntimeError('original host is not the actual candidate compositor')
ORIGINAL_HOST={'pid':compositor_pid, 'executable':str(compositor_exe),
               'start_ticks':Path(f'/proc/{compositor_pid}/stat').read_text().rsplit(')',1)[1].split()[19],
               'sha256':hashlib.sha256(compositor_exe.read_bytes()).hexdigest()}
if PHASE=='initial':
    fixture()
    for case in CASES:
        app=select(case)
        pixels(case)
        if case=='wallpaper':
            # Change actual source bytes at the same URI without a settings
            # write or synthetic event. Retain both real pixel observations.
            original=json.loads((OUT/'fixture-identity.json').read_text())
            COLORS.reverse()
            write_picture()
            PHASE='initial-replaced'
            pixels(case)
            (OUT/'replacement-identity.json').write_text(json.dumps({'before':original,'after_sha256':hashlib.sha256(PICTURE.read_bytes()).hexdigest(),'same_uri':PICTURE.as_uri()},indent=2))
            PHASE='initial'
        steps.append({'case':case,'settings_identity':app})
    (OUT/'initial-session.json').write_text(json.dumps(current_session,indent=2))
    (OUT/'initial-journey.json').write_text(json.dumps(steps,indent=2))
elif PHASE=='restarted':
    previous=json.loads((OUT/'initial-session.json').read_text())
    if (current_session['owner']==previous['owner'] or current_session['pid']==previous['pid']
            or current_session['compositor_pid']==previous['compositor_pid']
            or current_session['sha256']!=previous['sha256']):
        raise RuntimeError('candidate compositor/GTK shell did not genuinely restart with identical payload')
    pixels('vertical')
    app=select('vertical')
    pixels('vertical')
    (OUT/'restarted-session.json').write_text(json.dumps(current_session,indent=2))
else:raise RuntimeError('invalid phase')
