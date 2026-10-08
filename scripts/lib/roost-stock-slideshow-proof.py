#!/usr/bin/python3
"""Actual GNOME51 chooser and naturally advancing XML wallpaper proof."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import time
import struct
import sys
import zlib
import xml.etree.ElementTree as ET
import pyatspi
import gi
gi.require_version('GnomeBG','4.0')
gi.require_version('GdkPixbuf','2.0')
from gi.repository import Gio, GLib, GnomeBG, GdkPixbuf
OUT=Path('/out')
STATE=OUT/'compositor-state.json'
BUS=Gio.bus_get_sync(Gio.BusType.SESSION,None)
NAME='org.gnome.Settings'
EXE=Path('/usr/bin/gnome-control-center')
PHASE=sys.argv[1]
COLORS={'light-a':(240,0,0),'light-b':(0,0,240),'dark-a':(240,240,0),'dark-b':(120,0,160),'variant':(0,200,0)}
XML={style:OUT/(style+'-timeline.xml') for style in ['light','dark']}

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

def session_identity():
    owner=bus_call('GetNameOwner','(s)',('org.gnome.Shell',))
    pid=bus_call('GetConnectionUnixProcessID','(s)',(owner,))
    exe=Path(f'/proc/{pid}/exe').resolve()
    if exe!=Path('/candidate/usr/bin/roost-shell-gtk').resolve():raise RuntimeError('not actual candidate GTK shell')
    stat_text=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
    return {'owner':owner,'pid':pid,'start_time':stat_text[19],'executable':str(exe),'sha256':hashlib.sha256(exe.read_bytes()).hexdigest(),'compositor_pid':int(stat_text[1])}


def image(name,color):
    def chunk(kind,data):
        return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    rows=b''.join(b'\x00'+bytes(color)*64 for _ in range(32))
    (OUT/(name+'.png')).write_bytes(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',64,32,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(rows))+chunk(b'IEND',b''))


def timeline(style,reverse=False):
    root=ET.Element('background')
    start=ET.SubElement(root,'starttime')
    for tag,value in [('year','2020'),('month','1'),('day','1'),('hour','0'),('minute','0'),('second','0')]:
        ET.SubElement(start,tag).text=value
    a,b=style+'-a',style+'-b'
    if reverse:a,b=b,a
    def files(node,tag,name):
        parent=ET.SubElement(node,tag)
        ET.SubElement(parent,'size',{'width':'1280','height':'800'}).text=str(OUT/(name+'.png'))
        ET.SubElement(parent,'size',{'width':'640','height':'400'}).text=str(OUT/'variant.png')
    for fixed,first,last,duration in [(True,a,None,4),(False,a,b,8),(True,b,None,4),(False,b,a,8)]:
        node=ET.SubElement(root,'static' if fixed else 'transition')
        ET.SubElement(node,'duration').text=str(duration)
        files(node,'file' if fixed else 'from',first)
        if last:files(node,'to',last)
    # Genuine XML source replacement; no setting write or time substitution.
    temporary=XML[style].with_suffix('.tmp')
    ET.ElementTree(root).write(temporary,encoding='utf-8',xml_declaration=True)
    temporary.replace(XML[style])


def fixture():
    for name,color in COLORS.items():image(name,color)
    for style in XML:timeline(style)
    directory=OUT/'data/gnome-background-properties';directory.mkdir(parents=True)
    root=ET.Element('wallpapers');item=ET.SubElement(root,'wallpaper',{'deleted':'false'})
    for tag,value in [('name','Roost dynamic proof'),('filename',str(XML['light'])),('filename-dark',str(XML['dark'])),('options','stretched'),('shade_type','solid'),('pcolor','#00c800'),('scolor','#0000c8')]:
        ET.SubElement(item,tag).text=value
    ET.ElementTree(root).write(directory/'roost-dynamic.xml',encoding='utf-8',xml_declaration=True)


def activate_chooser(control, process, original, label):
    # GNOME51's two chooser FlowBoxes explicitly activate on a single click.
    # GTK4 supports window-relative bounds/hit queries, but neither GrabFocus
    # nor ScrollTo. Use actual pointer input after checking the real viewport.
    action = control.queryAction()
    actions = [action.getName(i) for i in range(action.nActions)]
    ancestors = []
    node = control
    for _ in range(32):
        node = node.parent
        if node is None:
            break
        ancestors.append(node)
        if node.getRoleName() == 'frame':
            break
    frames = [n for n in ancestors if n.getRoleName() == 'frame']
    panes = [n for n in ancestors if n.getRoleName() == 'scroll pane']
    if len(frames) != 1 or not panes:
        raise RuntimeError('chooser lacks actual bounded frame/scroll viewport')
    frame, pane = frames[0], panes[0]
    hosts = subprocess.check_output(['xdotool', 'search', '--onlyvisible', '--name', '^Smithay'], text=True).split()
    if len(hosts) != 1:
        raise RuntimeError('expected exactly one actual Smithay pointer host')
    host = hosts[0]
    pid = int(subprocess.check_output(['xdotool', 'getwindowpid', host], text=True))
    def host_guard():
        guard(process, original)
        executable = Path(f'/proc/{pid}/exe').resolve(strict=True)
        actual = {'pid': pid, 'executable': str(executable),
                  'start_ticks': Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19],
                  'sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}
        if actual != ORIGINAL_HOST or scene().get('focused_app_id') != NAME:
            raise RuntimeError('original compositor/Settings pointer identity changed')
        if int(subprocess.check_output(['xdotool', 'getwindowfocus'], text=True)) != int(host):
            raise RuntimeError('actual Smithay host does not own X input focus')
        return actual
    subprocess.run(['xdotool', 'windowfocus', '--sync', host], check=True, timeout=5)
    geometry = dict(line.split('=', 1) for line in subprocess.check_output(
        ['xdotool', 'getwindowgeometry', '--shell', host], text=True).splitlines())
    def bounds(node):
        node.clear_cache()
        r = node.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
        return [r.x, r.y, r.width, r.height]
    def measured():
        host_guard()
        fx, fy, fw, fh = scene()['focused_rect']
        r, v = bounds(control), bounds(pane)
        if min(r[2:]) <= 0 or min(v[2:]) <= 0:
            raise RuntimeError('actual chooser/viewport has invalid bounds')
        left, top = max(0, v[0]), max(0, v[1])
        right, bottom = min(fw, v[0]+v[2]), min(fh, v[1]+v[3])
        if right-left < 8 or bottom-top < 8:
            raise RuntimeError('actual scroll viewport is outside Settings')
        (OUT/f'{label}-pointer-measurement.json').write_text(json.dumps({
            'target': control.name, 'bounds': r, 'viewport_bounds': v,
            'focused_rect': [fx, fy, fw, fh], 'scene': scene()}, indent=2))
        return r, [left, top, right, bottom], [fx, fy, fw, fh]
    scrolls = []
    for _ in range(24):
        r, v, focused_rect = measured()
        cx, cy = r[0]+r[2]//2, r[1]+r[3]//2
        if v[0]+2 <= cx < v[2]-2 and v[1]+2 <= cy < v[3]-2:
            break
        if not v[0] <= cx < v[2]:
            raise RuntimeError('actual chooser requires unsupported horizontal scrolling')
        direction = 5 if cy >= v[3]-2 else 4
        wx, wy = focused_rect[0]+(v[0]+v[2])//2, focused_rect[1]+(v[1]+v[3])//2
        if not (0 <= wx < int(geometry['WIDTH']) and 0 <= wy < int(geometry['HEIGHT'])):
            raise RuntimeError('actual scroll point is outside the original host')
        before = r
        host_guard()
        subprocess.run(['xdotool', 'mousemove', '--window', host, str(wx), str(wy), 'click', str(direction)], check=True)
        wait_for(lambda: bounds(control) != before, 'genuine wheel did not change actual chooser bounds', 3)
        after, _, _ = measured()
        if (direction == 5 and after[1] >= before[1]) or (direction == 4 and after[1] <= before[1]):
            raise RuntimeError('genuine wheel did not move the target toward its viewport')
        scrolls.append({'before': before, 'after': after, 'wheel': direction, 'host_point': [wx, wy]})
    else:
        raise RuntimeError('actual chooser did not enter viewport within the bounded wheel budget')
    control.clear_cache()
    if not control.getState().contains(pyatspi.STATE_SHOWING):
        raise RuntimeError('actual chooser target is not showing')
    hit = frame.queryComponent().getAccessibleAtPoint(cx, cy, pyatspi.WINDOW_COORDS)
    hit_chain = []
    for _ in range(32):
        if hit is None:
            break
        hit_chain.append({'name': hit.name, 'role': hit.getRoleName()})
        if hit == control:
            break
        hit = hit.parent
    else:
        raise RuntimeError('bounded actual pointer hit ancestry exceeded')
    (OUT/f'{label}-pointer-hit.json').write_text(json.dumps({
        'target': control.name, 'window_point': [cx, cy],
        'hit_chain': hit_chain, 'matches_target': hit == control}, indent=2))
    if hit != control:
        raise RuntimeError('actual pointer hit does not identify the unique chooser target')
    fx, fy, _, _ = focused_rect
    x, y = fx+cx, fy+cy
    if not (0 <= x < int(geometry['WIDTH']) and 0 <= y < int(geometry['HEIGHT'])):
        raise RuntimeError('actual chooser click is outside the original host')
    actual = host_guard()
    subprocess.run(['scrot', '-o', str(OUT/f'{label}-before-click.png')], check=True)
    (OUT/f'{label}-pointer-host.json').write_text(json.dumps({
        'host': actual, 'window': int(host), 'actual_x_focus': int(host),
        'target': control.name, 'actions': actions, 'target_bounds': r,
        'viewport': v, 'hit_chain': hit_chain, 'scrolls': scrolls,
        'click': [x, y], 'scene_before_click': scene(),
        'settings_identity': original, 'activation_contract': 'single-click'}, indent=2))
    host_guard()
    if measured() != (r, v, focused_rect):
        raise RuntimeError('actual target/viewport moved before the single click')
    # Exactly one genuine selection click. Wheel setup never activates a tile.
    subprocess.run(['xdotool', 'mousemove', '--window', host, str(x), str(y), 'click', '1'], check=True)
    host_guard()


def select(style,choose=False):
    log=(OUT/f'{PHASE}-{style}-settings.log').open('w')
    process=subprocess.Popen([str(EXE),'background'],stdout=log,stderr=log)
    try:
        wait_for(lambda:bus_call('NameHasOwner','(s)',(NAME,)),'actual Settings bus missing')
        original=identity(process)
        wait_for(lambda:scene().get('focused_app_id')==NAME,'Settings did not receive actual focus')
        (OUT/f'{PHASE}-{style}-identity.json').write_text(json.dumps(original,indent=2))
        def find(target):
            matches=[c for c in controls(process,original,PHASE+'-'+style) if c.name==target]
            if len(matches)>1:raise RuntimeError('ambiguous actual Settings control')
            return matches[0] if matches else None
        def activate(target):
            wait_for(lambda:find(target) is not None,'actual Settings control missing: '+target,30)
            control=find(target)
            control.clear_cache()
            if target=='Roost dynamic proof':
                activate_chooser(control,process,original,PHASE+'-'+style)
            else:
                if not control.getState().contains(pyatspi.STATE_SHOWING):raise RuntimeError('actual style control not showing')
                action=control.queryAction()
                if action.nActions<1 or not action.doAction(0):raise RuntimeError('actual control action failed')
        if choose:activate('Roost dynamic proof')
        activate('Dark' if style=='dark' else 'Default')
        def saved():
            keys={key:subprocess.check_output(['gsettings','get','org.gnome.desktop.background',key],text=True).strip() for key in ['picture-uri','picture-uri-dark','picture-options']}
            scheme=subprocess.check_output(['gsettings','get','org.gnome.desktop.interface','color-scheme'],text=True).strip()
            (OUT/f'{PHASE}-{style}-keys.json').write_text(json.dumps({'background':keys,'color-scheme':scheme},indent=2))
            return keys['picture-uri']==repr(XML['light'].as_uri()) and keys['picture-uri-dark']==repr(XML['dark'].as_uri()) and keys['picture-options']==repr('stretched') and scheme==repr('prefer-dark' if style=='dark' else 'default')
        wait_for(saved,'real chooser/style did not persist XML and color scheme')
        guard(process,original)
        if not find('Roost dynamic proof').getState().contains(pyatspi.STATE_CHECKED):raise RuntimeError('actual XML chooser item not selected')
        subprocess.run(['scrot','-o',str(OUT/f'{PHASE}-{style}-settings.png')],check=True)
    finally:
        process.terminate();process.wait(timeout=10);log.close()
    wait_for(lambda:not bus_call('NameHasOwner','(s)',(NAME,)),'Settings did not exit')
    wait_for(lambda:not scene()['windows'],'unobstructed desktop contains app')


def reference(style):
    show=GnomeBG.BGSlideShow.new(str(XML[style]))
    if not show.load():raise RuntimeError('actual GNOME51 slideshow reference failed to load')
    return show


def observe(style,label,required=None):
    show=reference(style)
    seen={};samples=[];wrapped=False;deadline=time.monotonic()+85
    while time.monotonic()<deadline:
        paint_before=scene().get('background_paint')
        before=time.time()
        progress,duration,fixed,first,last=show.get_current_slide(1280,800)
        reference_after=time.time()
        path=OUT/f'{PHASE}-{label}-latest.png'
        capture_before=time.time()
        subprocess.run(['scrot','-o',str(path)],check=True)
        after=time.time()
        paint_after=scene().get('background_paint')
        pb=GdkPixbuf.Pixbuf.new_from_file(str(path));data=pb.get_pixels();offset=400*pb.get_rowstride()+640*pb.get_n_channels()
        actual=tuple(data[offset:offset+3])
        first_name=Path(first).stem
        last_name=Path(last).stem if last else None
        ca=COLORS[first_name];cb=COLORS[last_name] if last else ca
        stable=paint_before is not None and paint_before==paint_after
        sampled_progress=progress
        age=None;progress_low=None;progress_high=None
        time_qualified=False
        if stable:
            sample_wall=paint_before['sample_wall']
            age=after-sample_wall
            same_paths=paint_before['from']==first and paint_before['to']==(last or None)
            if fixed:
                sampled_progress=0.0
                progress_qualified=paint_before['progress']==0.0
            else:
                progress_low=progress-(reference_after-sample_wall)/duration
                progress_high=progress-(before-sample_wall)/duration
                sampled_progress=(progress_low+progress_high)/2
                progress_qualified=progress_low-.002<=paint_before['progress']<=progress_high+.002
            time_qualified=(same_paths and progress_qualified and 0<=age<=paint_before['interval']+.5
                            and after-capture_before<=.5)
        want=tuple(round(a*(1-sampled_progress)+b*sampled_progress) for a,b in zip(ca,cb))
        entry={'reference_before_wall':before,'reference_after_wall':reference_after,
               'capture_before_wall':capture_before,'capture_after_wall':after,
               'before_wall':before,'after_wall':after,'paint_before':paint_before,'paint_after':paint_after,
               'actual_sample_age':age,'reference_progress_at_sample_bounds':[progress_low,progress_high],
               'time_qualified':time_qualified,
               'reference':{'progress':progress,'duration':duration,'fixed':fixed,'from':first,'to':last},
               'actual':actual,'expected_reference_at_actual_sample':want}
        samples.append(entry)
        # Exact pixels correspond to the independently bounded GNOME reference
        # progress at the candidate's retained real sample wall time. A stale
        # frame, wrong phase, source epoch mismatch or slow capture cannot pass.
        correct=time_qualified and max(abs(a-b) for a,b in zip(actual,want))<=2
        state=('static-'+first_name) if fixed else ('blend-'+first_name+'-'+last_name if .35<=progress<=.65 else None)
        if correct and state:
            if state not in seen:
                seen[state]=entry
                path.replace(OUT/f'{PHASE}-{label}-{state}.png')
            elif fixed and before-seen[state]['before_wall']>=22:
                wrapped=True
                path.replace(OUT/f'{PHASE}-{label}-cycle-wrap.png')
        (OUT/f'{PHASE}-{label}-natural-clock-pixels.json').write_text(json.dumps({'samples':samples,'qualified_states':seen,'natural_cycle_wrap':wrapped},indent=2))
        if required:
            if required in seen:return
        elif wrapped and (style+'-a') in ''.join(k for k in seen if k.startswith('static-')) and (style+'-b') in ''.join(k for k in seen if k.startswith('static-')) and sum(k.startswith('blend-') for k in seen)>=2:
            return
        time.sleep(.15)
    raise RuntimeError('naturally advancing actual pixels did not qualify required reference phases')


wait_for(lambda:bus_call('NameHasOwner','(s)',('org.gnome.Shell',)),'actual GTK shell missing',30)
current=session_identity()
compositor_pid=current['compositor_pid']
compositor_exe=Path(f'/proc/{compositor_pid}/exe').resolve(strict=True)
if compositor_exe != Path('/candidate/usr/bin/roost-compositor').resolve(strict=True):
    raise RuntimeError('original host is not the actual candidate compositor')
ORIGINAL_HOST={'pid':compositor_pid,'executable':str(compositor_exe),
               'start_ticks':Path(f'/proc/{compositor_pid}/stat').read_text().rsplit(')',1)[1].split()[19],
               'sha256':hashlib.sha256(compositor_exe.read_bytes()).hexdigest()}
if PHASE=='initial':
    fixture()
    select('light',True)
    observe('light','light-cycle')
    select('dark')
    observe('dark','dark-cycle')
    old=hashlib.sha256((OUT/'dark-a.png').read_bytes()).hexdigest()
    COLORS['dark-a']=(0,240,240);image('dark-a',COLORS['dark-a'])
    observe('dark','same-image-uri','static-dark-a')
    (OUT/'image-replacement.json').write_text(json.dumps({'same_uri':(OUT/'dark-a.png').as_uri(),'before_sha256':old,'after_sha256':hashlib.sha256((OUT/'dark-a.png').read_bytes()).hexdigest()},indent=2))
    good=XML['dark'].read_bytes()
    for negative,payload in [('forbidden-entity',b"<!DOCTYPE background [<!ENTITY a 'x'>]><background><static><duration>4</duration><file>&a;</file></static></background>"),('oversized',b' '* (1024*1024+1))]:
        XML['dark'].write_bytes(payload)
        path=OUT/f'{PHASE}-{negative}.png'
        def rejected():
            subprocess.run(['scrot','-o',str(path)],check=True)
            pb=GdkPixbuf.Pixbuf.new_from_file(str(path));offset=400*pb.get_rowstride()+640*pb.get_n_channels()
            actual=tuple(pb.get_pixels()[offset:offset+3])
            (OUT/f'{PHASE}-{negative}.json').write_text(json.dumps({'actual':actual,'expected_solid_fallback':[0,200,0],'xml_bytes':len(payload),'xml_sha256':hashlib.sha256(payload).hexdigest()},indent=2))
            return actual==(0,200,0)
        wait_for(rejected,'unsafe XML was not rejected to bounded fallback',30)
    XML['dark'].write_bytes(good)
    old=hashlib.sha256(XML['dark'].read_bytes()).hexdigest();timeline('dark',True)
    observe('dark','same-xml-uri')
    (OUT/'xml-replacement.json').write_text(json.dumps({'same_uri':XML['dark'].as_uri(),'before_sha256':old,'after_sha256':hashlib.sha256(XML['dark'].read_bytes()).hexdigest()},indent=2))
    (OUT/'initial-session.json').write_text(json.dumps(current,indent=2))
elif PHASE=='restarted':
    previous=json.loads((OUT/'initial-session.json').read_text())
    if current['pid']==previous['pid'] or current['owner']==previous['owner'] or current['compositor_pid']==previous['compositor_pid'] or current['sha256']!=previous['sha256']:raise RuntimeError('candidate session did not genuinely restart')
    COLORS['dark-a']=(0,240,240)
    # Observe the persisted XML before launching Settings after genuine restart.
    observe('dark','persisted-restart')
    select('dark')
    (OUT/'restarted-session.json').write_text(json.dumps(current,indent=2))
else:raise RuntimeError('invalid proof phase')
