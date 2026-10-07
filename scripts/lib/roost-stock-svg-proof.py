#!/usr/bin/python3
"""Actual GNOME51 SVG chooser, independent native-loader pixels and restart."""
import base64
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import time
import sys
import struct
import zlib
import xml.etree.ElementTree as ET
import pyatspi
import gi
gi.require_version('GdkPixbuf','2.0')
from gi.repository import Gio, GLib, GdkPixbuf
OUT=Path('/out')
STATE=OUT/'compositor-state.json'
BUS=Gio.bus_get_sync(Gio.BusType.SESSION,None)
NAME='org.gnome.Settings'
EXE=Path('/usr/bin/gnome-control-center')
PHASE=sys.argv[1]
SVG={style:OUT/(style+'.svg') for style in ['light','dark']}

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


def write_svg(style, color):
    def chunk(kind,data):
        return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',2,2,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+bytes([240,240,0])*2)*2))+chunk(b'IEND',b'')
    embedded=base64.b64encode(png).decode()
    document=f'''<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1280 800">
<style>.base {{fill:rgb{color};}} .ink {{fill:#f0f0f0; font-family:sans-serif; font-size:48px;}}</style>
<rect width="1280" height="800" class="base"/>
<defs><rect id="patch" width="120" height="120" fill="#00c800"/></defs>
<use href="#patch" x="900" y="200"/>
<rect x="150" y="300" width="200" height="160" fill="#c80a14" fill-opacity="0.5"/>
<image x="700" y="500" width="100" height="100" href="data:image/png;base64,{embedded}"/>
<text x="80" y="140" class="ink">GNOME SVG {style}</text>
</svg>'''
    temporary=SVG[style].with_suffix('.tmp');temporary.write_text(document);temporary.replace(SVG[style])


def fixture():
    write_svg('light',(25,87,200));write_svg('dark',(120,0,160))
    directory=OUT/'data/gnome-background-properties';directory.mkdir(parents=True)
    root=ET.Element('wallpapers');item=ET.SubElement(root,'wallpaper',{'deleted':'false'})
    for tag,value in [('name','Roost SVG proof'),('filename',str(SVG['light'])),('filename-dark',str(SVG['dark'])),('options','centered'),('shade_type','solid'),('pcolor','#00c800'),('scolor','#0000c8')]:
        ET.SubElement(item,tag).text=value
    ET.ElementTree(root).write(directory/'roost-svg.xml',encoding='utf-8',xml_declaration=True)


def select(style,choose=False):
    log=(OUT/f'{PHASE}-{style}-settings.log').open('w')
    process=subprocess.Popen([str(EXE),'background'],stdout=log,stderr=log)
    try:
        wait_for(lambda:bus_call('NameHasOwner','(s)',(NAME,)),'actual Settings bus missing')
        original=identity(process)
        wait_for(lambda:scene().get('focused_app_id')==NAME,'actual Settings did not receive focus')
        (OUT/f'{PHASE}-{style}-identity.json').write_text(json.dumps(original,indent=2))
        def find(target):
            matches=[c for c in controls(process,original,PHASE+'-'+style) if c.name==target]
            if len(matches)>1:raise RuntimeError('nonunique actual Settings control')
            return matches[0] if matches else None
        def activate(target,chooser=False):
            wait_for(lambda:find(target) is not None,'actual Settings control missing: '+target,30)
            control=find(target);control.clear_cache()
            if not control.getState().contains(pyatspi.STATE_SHOWING):
                control.queryComponent().scrollTo(pyatspi.SCROLL_ANYWHERE);drain();control.clear_cache()
            if not control.getState().contains(pyatspi.STATE_SHOWING):raise RuntimeError('actual control not showing')
            if chooser:activate_chooser(control,process,original,PHASE+'-'+style)
            else:
                action=control.queryAction()
                if action.nActions!=1 or action.getName(0)!='click' or not action.doAction(0):
                    raise RuntimeError('actual style button action failed')
        if choose:activate('Roost SVG proof',True)
        activate('Dark' if style=='dark' else 'Default')
        def saved():
            keys={schema:{key:subprocess.check_output(['gsettings','get',schema,key],text=True).strip() for key in ['picture-uri','picture-options']} for schema in ['org.gnome.desktop.background','org.gnome.desktop.screensaver']}
            dark=subprocess.check_output(['gsettings','get','org.gnome.desktop.background','picture-uri-dark'],text=True).strip()
            scheme=subprocess.check_output(['gsettings','get','org.gnome.desktop.interface','color-scheme'],text=True).strip()
            (OUT/f'{PHASE}-{style}-keys.json').write_text(json.dumps({'keys':keys,'dark':dark,'scheme':scheme},indent=2))
            return (all(k['picture-uri']==repr(SVG['light'].as_uri()) and k['picture-options']==repr('centered') for k in keys.values()) and dark==repr(SVG['dark'].as_uri()) and scheme==repr('prefer-dark' if style=='dark' else 'default'))
        wait_for(saved,'real chooser/style did not persist SVG desktop+lock/style keys')
        guard(process,original)
        if not find('Roost SVG proof').getState().contains(pyatspi.STATE_CHECKED):raise RuntimeError('actual SVG chooser item unchecked')
        subprocess.run(['scrot','-o',str(OUT/f'{PHASE}-{style}-settings.png')],check=True)
    finally:
        process.terminate();process.wait(timeout=10);log.close()
    wait_for(lambda:not bus_call('NameHasOwner','(s)',(NAME,)),'Settings did not exit')
    wait_for(lambda:not scene()['windows'],'desktop still contains app windows')


def pixels(style,label):
    # Independent real installed GdkPixbuf reference, not the candidate helper.
    source=SVG[style].read_bytes();loader=GdkPixbuf.PixbufLoader.new_with_type('svg')
    loader.write(source);loader.close();reference=loader.get_pixbuf()
    if reference.get_width()!=1280 or reference.get_height()!=800:raise RuntimeError('actual SVG intrinsic/viewBox reference changed')
    reference.savev(str(OUT/f'{PHASE}-{label}-reference.png'),'png',[],[])
    ref=reference.get_pixels();rs=reference.get_rowstride();rc=reference.get_n_channels()
    # Include native font ink and CSS/use/data-image/transparency patches.
    points={(x,y) for y in range(60,800,23) for x in range(10,1280,17)}
    ink=[(x,y) for y in range(95,145) for x in range(80,600) if tuple(ref[y*rs+x*rc:y*rs+x*rc+3])==(240,240,240)]
    if len(ink)<100:raise RuntimeError('independent native fonts produced no actual text ink')
    points.update(ink[::max(1,len(ink)//100)])
    path=OUT/f'{PHASE}-{label}-desktop.png'
    def correct():
        subprocess.run(['scrot','-o',str(path)],check=True)
        image=GdkPixbuf.Pixbuf.new_from_file(str(path));data=image.get_pixels();stride=image.get_rowstride();channels=image.get_n_channels()
        if image.get_width()!=1280 or image.get_height()!=800:raise RuntimeError('wrong actual screenshot dimensions')
        observed=[]
        for x,y in sorted(points):
            expected=tuple(ref[y*rs+x*rc:y*rs+x*rc+3]);actual=tuple(data[y*stride+x*channels:y*stride+x*channels+3])
            observed.append({'point':[x,y],'actual':actual,'reference':expected})
        (OUT/f'{PHASE}-{label}-pixels.json').write_text(json.dumps({'source_sha256':hashlib.sha256(source).hexdigest(),'reference_pixel_sha256':hashlib.sha256(ref).hexdigest(),'screenshot_sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'samples':observed,'font_ink_pixels':len(ink)},indent=2))
        return all(max(abs(a-b) for a,b in zip(p['actual'],p['reference']))<=1 for p in observed)
    wait_for(correct,'actual desktop SVG pixels differ from the actual independent native-loader reference',35)
    (OUT/f'{PHASE}-{label}-scene.json').write_text(json.dumps(scene(),indent=2))


def fallback(label):
    def correct():
        path=OUT/f'{PHASE}-{label}-fallback.png';subprocess.run(['scrot','-o',str(path)],check=True)
        image=GdkPixbuf.Pixbuf.new_from_file(str(path));data=image.get_pixels();stride=image.get_rowstride();channels=image.get_n_channels()
        return all(tuple(data[y*stride+x*channels:y*stride+x*channels+3])==(0,200,0) for x,y in [(10,780),(300,780),(1270,780)])
    wait_for(correct,'invalid/excessive SVG did not fall back to selected primary color',35)


wait_for(lambda:bus_call('NameHasOwner','(s)',('org.gnome.Shell',)),'actual GTK shell did not start',30)
current_session=session_identity();pid=current_session['compositor_pid'];exe=Path(f'/proc/{pid}/exe').resolve(strict=True)
if exe!=Path('/candidate/usr/bin/roost-compositor').resolve(strict=True):raise RuntimeError('not actual candidate compositor')
ORIGINAL_HOST={'pid':pid,'executable':str(exe),'start_ticks':Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()[19],'sha256':hashlib.sha256(exe.read_bytes()).hexdigest()}
if PHASE=='initial':
    fixture();select('light',True);pixels('light','light')
    before=hashlib.sha256(SVG['light'].read_bytes()).hexdigest()
    write_svg('light',(200,10,20));pixels('light','same-uri-rewrite')
    (OUT/'same-uri-rewrite.json').write_text(json.dumps({'uri':SVG['light'].as_uri(),'before_sha256':before,'after_sha256':hashlib.sha256(SVG['light'].read_bytes()).hexdigest()},indent=2))
    select('dark');pixels('dark','dark')
    saved=SVG['dark'].read_bytes()
    SVG['dark'].write_text('<svg');fallback('malformed')
    SVG['dark'].write_text('<svg xmlns="http://www.w3.org/2000/svg" width="90000" height="90000"/>');fallback('dimension-bound')
    SVG['dark'].write_bytes(saved);pixels('dark','restored')
    # Genuine lock, then retain actual unobstructed corner pixels. These points
    # are >20*sigma from all SVG patches, so the specified blur cannot affect
    # them. The current screensaver URI is the rewritten light variant.
    subprocess.run(['gdbus','call','--session','--dest','org.gnome.ScreenSaver','--object-path','/org/gnome/ScreenSaver','--method','org.gnome.ScreenSaver.Lock'],check=True)
    wait_for(lambda:scene()['locked'],'actual compositor did not lock')
    def locked_pixels():
        path=OUT/'initial-lock.png';subprocess.run(['scrot','-o',str(path)],check=True)
        image=GdkPixbuf.Pixbuf.new_from_file(str(path));data=image.get_pixels();stride=image.get_rowstride();channels=image.get_n_channels()
        observed=[]
        for x,y in [(10,780),(1270,780)]:
            actual=tuple(data[y*stride+x*channels:y*stride+x*channels+3]);observed.append({'point':[x,y],'actual':actual,'expected':(130,6,13)})
        (OUT/'initial-lock-pixels.json').write_text(json.dumps({'samples':observed,'scene':scene(),'screensaver_uri':SVG['light'].as_uri()},indent=2))
        return all(max(abs(a-b) for a,b in zip(v['actual'],v['expected']))<=2 for v in observed)
    wait_for(locked_pixels,'actual SVG lock background pixels did not match screensaver source/dimming',35)
    (OUT/'initial-session.json').write_text(json.dumps(current_session,indent=2))
elif PHASE=='restarted':
    previous=json.loads((OUT/'initial-session.json').read_text())
    if (current_session['owner']==previous['owner'] or current_session['pid']==previous['pid'] or current_session['compositor_pid']==previous['compositor_pid'] or current_session['sha256']!=previous['sha256']):raise RuntimeError('candidate compositor/GTK shell did not genuinely restart')
    # Observe persisted dark pixels BEFORE reopening Settings.
    pixels('dark','persisted-before-settings');select('dark');pixels('dark','persisted-after-settings')
    (OUT/'restarted-session.json').write_text(json.dumps(current_session,indent=2))
else:raise RuntimeError('invalid phase')

def session_identity():
    owner=bus_call('GetNameOwner','(s)',('org.gnome.Shell',))
    pid=bus_call('GetConnectionUnixProcessID','(s)',(owner,))
    exe=Path(f'/proc/{pid}/exe').resolve()
    if exe!=Path('/candidate/usr/bin/roost-shell-gtk').resolve():raise RuntimeError('not actual candidate GTK shell')
    stat_text=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
    return {'owner':owner,'pid':pid,'start_time':stat_text[19],'executable':str(exe),'sha256':hashlib.sha256(exe.read_bytes()).hexdigest(),'compositor_pid':int(stat_text[1])}
