#!/usr/bin/env python3
"""Actual session-bus contract receipts; native VM case has no fake backlight."""
import argparse
import json
import os
import stat
from pathlib import Path
import time
import xml.etree.ElementTree as ET
from gi.repository import Gio, GLib

NAME='org.gnome.Shell.Brightness'
PATH='/org/gnome/Shell/Brightness'

def call(bus,dest,path,interface,method,args=None):
    return bus.call_sync(dest,path,interface,method,args,None,Gio.DBusCallFlags.NONE,5000,None)

def schema(xml):
    if len(xml.encode())>65536:raise RuntimeError('introspection exceeds bound')
    interfaces=[x for x in ET.fromstring(xml).findall('interface') if x.get('name')==NAME]
    if len(interfaces)!=1:raise RuntimeError('actual brightness interface missing/duplicated')
    interface=interfaces[0]
    methods={m.get('name'):[(a.get('type'),a.get('direction','in')) for a in m.findall('arg')] for m in interface.findall('method')}
    if len(interface.findall('method'))!=2 or methods!={'SetDimming':[('b','in')],'SetAutoBrightnessTarget':[('d','in')]}:raise RuntimeError('brightness method signature mismatch')
    if [(p.get('name'),p.get('type'),p.get('access')) for p in interface.findall('property')]!=[('HasBrightnessControl','b','read')]:raise RuntimeError('brightness property mismatch')
    if [(s.get('name'),len(s.findall('arg'))) for s in interface.findall('signal')]!=[('BrightnessChanged',0)]:raise RuntimeError('brightness signal mismatch')

def process(pid,expected_executable):
    if pid<=0:raise RuntimeError('invalid process ID')
    path=Path(f'/proc/{pid}')
    uid=path.stat().st_uid
    executable=os.readlink(path/'exe')
    if uid==0 or uid!=os.getuid() or executable!=expected_executable:raise RuntimeError('process UID or exact executable mismatch')
    with (path/'stat').open('rb') as stream:raw=stream.read(4097)
    if len(raw)>4096:raise RuntimeError('process stat exceeds bound')
    fields=raw.rsplit(b')',1)[1].split()
    return dict(pid=pid,uid=uid,start_ticks=int(fields[19]),executable=executable,parent_pid=int(fields[1]))

def principal(bus,expected_executable,expected=None):
    db='org.freedesktop.DBus';path='/org/freedesktop/DBus'
    owner=call(bus,db,path,db,'GetNameOwner',GLib.Variant('(s)',(NAME,))).unpack()[0]
    shell=call(bus,db,path,db,'GetNameOwner',GLib.Variant('(s)',('org.gnome.Shell',))).unpack()[0]
    if owner!=shell or not owner.startswith(':'):raise RuntimeError('brightness and original shell unique owners differ')
    pid=call(bus,db,path,db,'GetConnectionUnixProcessID',GLib.Variant('(s)',(owner,))).unpack()[0]
    uid=call(bus,db,path,db,'GetConnectionUnixUser',GLib.Variant('(s)',(owner,))).unpack()[0]
    original=process(pid,expected_executable)
    if uid==0 or uid!=original['uid']:raise RuntimeError('shell bus and process credentials differ')
    parent=original['parent_pid']
    parent_exe=os.readlink(f'/proc/{parent}/exe')
    if Path(parent_exe).name!='roost-compositor':raise RuntimeError('shell is not original compositor child')
    compositor=process(parent,parent_exe)
    result=dict(owner=owner,**original,compositor=compositor)
    if expected and any(result.get(key)!=value for key,value in expected.items()):raise RuntimeError('original native lifecycle principal mismatch')
    return result

def available(bus,owner):
    value=call(bus,owner,PATH,'org.freedesktop.DBus.Properties','Get',GLib.Variant('(ss)',(NAME,'HasBrightnessControl'))).unpack()[0]
    if type(value)is not bool:raise RuntimeError('capability is not boolean')
    return value

def scalar(path):
    fd=os.open(path,os.O_RDONLY|os.O_NONBLOCK|os.O_NOFOLLOW|os.O_CLOEXEC)
    try:
        before=os.fstat(fd)
        if not stat.S_ISREG(before.st_mode):raise RuntimeError('scalar is not regular')
        raw=os.read(fd,65)
        after=os.stat(path,follow_symlinks=False)
        if (before.st_dev,before.st_ino)!=(after.st_dev,after.st_ino) or not stat.S_ISREG(after.st_mode):raise RuntimeError('scalar identity changed')
        if len(raw)>64:raise RuntimeError('scalar exceeds bound')
        value=int(raw)
        if value<0 or value>4294967295:raise RuntimeError('scalar outside u32 range')
        return value
    finally:os.close(fd)

def retain(path,result):
    raw=json.dumps(result,indent=2).encode()
    if len(raw)>1048576:raise RuntimeError('receipt exceeds bound')
    fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW|os.O_CLOEXEC,0o600)
    try:
        before=os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_uid!=os.getuid():raise RuntimeError('receipt ownership/type mismatch')
        with os.fdopen(fd,'wb',closefd=False) as stream:stream.write(raw);stream.flush()
        after=os.stat(path,follow_symlinks=False)
        if (before.st_dev,before.st_ino)!=(after.st_dev,after.st_ino) or after.st_size!=len(raw):raise RuntimeError('receipt identity/size changed')
    finally:os.close(fd)

def backlight_type(path):
    fd=os.open(path,os.O_RDONLY|os.O_NONBLOCK|os.O_NOFOLLOW|os.O_CLOEXEC)
    try:
        before=os.fstat(fd)
        if not stat.S_ISREG(before.st_mode):raise RuntimeError('backlight type is nonregular')
        raw=os.read(fd,17);after=os.stat(path,follow_symlinks=False)
        if len(raw)>16 or not stat.S_ISREG(after.st_mode) or (before.st_dev,before.st_ino)!=(after.st_dev,after.st_ino):raise RuntimeError('backlight type identity/bound mismatch')
        kind=raw.decode().strip()
        if kind not in ('raw','firmware','platform'):raise RuntimeError('unknown backlight type')
        return kind
    finally:os.close(fd)

def floor(maximum,kind):
    if kind not in ('raw','firmware','platform') or maximum<=0:raise RuntimeError('invalid real backlight type/range')
    return 0 if kind=='raw' and maximum<99 else max(1,maximum//100)

def controlled_manifest(path):
    fd=os.open(path,os.O_RDONLY|os.O_NONBLOCK|os.O_NOFOLLOW|os.O_CLOEXEC)
    try:
        before=os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_uid!=os.getuid():raise RuntimeError('controlled manifest owner/type mismatch')
        raw=os.read(fd,65537);after=os.stat(path,follow_symlinks=False)
        if len(raw)>65536 or not stat.S_ISREG(after.st_mode) or (before.st_dev,before.st_ino)!=(after.st_dev,after.st_ino):raise RuntimeError('controlled manifest identity/bound mismatch')
        outputs=json.loads(raw)
        if not isinstance(outputs,list) or len(outputs)>64:raise RuntimeError('controlled manifest count/type mismatch')
        for output in outputs:
            if not isinstance(output,dict) or set(output)!={'name','drm_device','connector_id','connector_sysfs','connector_device','connector_inode'}:raise RuntimeError('controlled manifest schema mismatch')
            if output['drm_device']!=0 or not isinstance(output['name'],str) or len(output['name'])>512 or not isinstance(output['connector_sysfs'],str) or len(output['connector_sysfs'])>512:raise RuntimeError('controlled manifest field bounds/source mismatch')
        return outputs
    finally:os.close(fd)

def overridden_backlight_environment():
    return any(key in os.environ for key in ('ROOST_BACKLIGHT_ROOT','ROOST_BACKLIGHT_TEST_CONNECTORS'))

def signal_receipt(signals,overflow):
    if len(signals)>=512:overflow[0]=True
    else:signals.append(time.monotonic_ns())

def drain(context,deadline,budget):
    while context.pending():
        if budget[0]<=0 or time.monotonic()>=deadline:raise RuntimeError('bounded signal dispatch exhausted')
        context.iteration(False);budget[0]-=1

def main():
    parser=argparse.ArgumentParser();parser.add_argument('--out',required=True);parser.add_argument('--controlled-root');parser.add_argument('--device');parser.add_argument('--shell-executable',required=True);parser.add_argument('--shell-pid',type=int);parser.add_argument('--shell-start',type=int);parser.add_argument('--compositor-pid',type=int);parser.add_argument('--compositor-start',type=int);parser.add_argument('--session-id')
    args=parser.parse_args();bus=Gio.bus_get_sync(Gio.BusType.SESSION,None)
    identity=principal(bus,args.shell_executable)
    if not args.controlled_root:
        if not args.session_id or identity['pid']!=args.shell_pid or identity['start_ticks']!=args.shell_start or identity['compositor']['pid']!=args.compositor_pid or identity['compositor']['start_ticks']!=args.compositor_start:raise RuntimeError('native original session/process inventory mismatch')
    receipts=[];signals=[];signal_overflow=[False]
    owner=identity['owner']
    subscription=bus.signal_subscribe(identity['owner'],NAME,'BrightnessChanged',PATH,None,Gio.DBusSignalFlags.NONE,lambda *args:signal_receipt(signals,signal_overflow))
    text=call(bus,owner,PATH,'org.freedesktop.DBus.Introspectable','Introspect').unpack()[0];schema(text)
    root=Path(args.controlled_root) if args.controlled_root else Path('/sys/class/backlight')
    if not args.controlled_root and overridden_backlight_environment():raise RuntimeError('native proof cannot accept overridden backlight')
    entries=[]
    if root.exists():
        for index,entry in enumerate(root.iterdir()):
            if index>=64:raise RuntimeError('actual backlight inventory exceeds bound')
            entries.append(dict(name=entry.name,path=str(entry.resolve())))
    def request(method,value,expected_failure=False):
        if signal_overflow[0]:raise RuntimeError('original signal receipt overflow')
        before=available(bus,owner);stamp=time.monotonic_ns();failed=None;signal_before=len(signals)
        try:call(bus,owner,PATH,NAME,method,GLib.Variant('(b)' if method=='SetDimming' else '(d)',(value,)))
        except GLib.Error as error:failed=Gio.DBusError.get_remote_error(error) or f'{error.domain}:{error.code}'
        if bool(failed)!=expected_failure:raise RuntimeError(f'{method}: expected_failure={expected_failure}, actual={failed}')
        context=GLib.MainContext.default()
        until=time.monotonic()+0.15;budget=[100]
        while time.monotonic()<until:
            drain(context,until,budget)
            time.sleep(0.005)
        if signal_overflow[0]:raise RuntimeError('original signal receipt overflow')
        if len(signals)!=signal_before:raise RuntimeError('policy/reset/refusal incorrectly emitted a user BrightnessChanged signal')
        if principal(bus,args.shell_executable)!=identity:raise RuntimeError('original shell principal changed')
        receipts.append(dict(method=method,value='NaN' if isinstance(value,float) and value!=value else value,
                             before_capability=before,after_capability=available(bus,owner),started_ns=stamp,ended_ns=time.monotonic_ns(),error=failed,user_signals_before=signal_before,user_signals_after=len(signals)))
    result=dict(principal=identity,session_id=args.session_id,native=not bool(args.controlled_root),backlight_root=str(root),backlights=entries,observations=receipts,
                limitation='Actual interface and unsupported-hardware VM proof only; controlled sysfs/logind policy receipts are synthetic, not physical hardware qualification.')
    primary=None;settings_restore=None
    try:
        if not args.controlled_root:
            if entries or available(bus,owner):raise RuntimeError('unsupported-VM fixture requires actual no-backlight inventory and false capability')
            request('SetDimming',False);request('SetAutoBrightnessTarget',-1.0)
            request('SetDimming',True,True);request('SetAutoBrightnessTarget',0.8,True)
            request('SetAutoBrightnessTarget',float('nan'),True)
        else:
            if not args.device or not available(bus,owner):raise RuntimeError('controlled associated backlight missing')
            device=root/args.device
            def read(name):return scalar(device/name)
            maximum=read('max_brightness');kind=backlight_type(device/'type');minimum=floor(maximum,kind)
            manifest=controlled_manifest(os.environ['ROOST_BACKLIGHT_TEST_CONNECTORS'])
            parent=(device/'device').resolve();metadata=parent.stat()
            if len(manifest)!=1 or manifest[0]['connector_sysfs']!=str(parent) or (manifest[0]['connector_device'],manifest[0]['connector_inode'])!=(metadata.st_dev,metadata.st_ino) or manifest[0]['connector_id']!=scalar(parent/'connector_id'):raise RuntimeError('controlled actual associated authority mismatch')
            result['controlled_authority']=manifest
            baseline=read('brightness');relative=(baseline-minimum)/(maximum-minimum)
            def observe(expected):
                actual=read('brightness');receipts[-1]['actual_brightness']=actual;receipts[-1]['expected_brightness']=expected
                if actual!=expected:raise RuntimeError(f'actual brightness={actual}, expected={expected}')
            # Source policy's ordinary installed idle setting, not an invented target.
            settings=Gio.Settings.new('org.gnome.settings-daemon.plugins.power');idle=settings.get_int('idle-brightness')/100
            absolute=lambda level:minimum+int((maximum-minimum)*max(0,min(1,level))+0.5)
            request('SetDimming',True);observe(absolute(min(idle,relative)))
            saved_idle=settings.get_int('idle-brightness');settings_restore=(settings,saved_idle)
            changed_idle=20 if saved_idle!=20 else 40
            if not settings.set_int('idle-brightness',changed_idle):raise RuntimeError('live idle setting refused')
            Gio.Settings.sync()
            deadline=time.monotonic()+5
            expected=absolute(min(changed_idle/100,relative))
            while read('brightness')!=expected and time.monotonic()<deadline:time.sleep(0.02)
            receipts.append(dict(setting='idle-brightness',before=saved_idle,after=changed_idle,actual_brightness=read('brightness'),expected_brightness=expected))
            if read('brightness')!=expected:raise RuntimeError('live dimmed idle setting did not reach actual backlight')
            if not settings.set_int('idle-brightness',saved_idle):raise RuntimeError('live idle restore refused')
            Gio.Settings.sync()
            deadline=time.monotonic()+5
            while read('brightness')!=absolute(min(idle,relative)) and time.monotonic()<deadline:time.sleep(0.02)
            if read('brightness')!=absolute(min(idle,relative)):raise RuntimeError('live idle restoration did not reach actual backlight')
            request('SetDimming',False);observe(baseline)
            request('SetAutoBrightnessTarget',0.8);observe(absolute(0.8+relative-0.5))
            request('SetAutoBrightnessTarget',-1.0);observe(baseline)
            request('SetAutoBrightnessTarget',float('nan'),True);observe(baseline)
            # Genuine session-bus error paths against the controlled logind fixture.
            # These are adapter tests, never evidence of physical hardware.
            mode=root/'.roost-brightness-failure'
            for failure in ['deny','mismatch']:
                mode.write_text(failure)
                try:
                    request('SetDimming',True,True)
                    observe(baseline if failure=='deny' else absolute(min(idle,relative))+1)
                finally:mode.unlink()
                request('SetDimming',False);observe(baseline)
        result['pass']=True
    except Exception as error:
        primary=error;result['pass']=False;result['error']=str(error)
    finally:
        cleanup=[]
        if settings_restore:
            settings,saved_idle=settings_restore
            try:
                if not settings.set_int('idle-brightness',saved_idle):raise RuntimeError('original idle setting restoration refused')
                Gio.Settings.sync()
                if settings.get_int('idle-brightness')!=saved_idle:raise RuntimeError('original idle setting restoration failed')
            except Exception as error:cleanup.append(str(error))
        for method,value in [('SetDimming',False),('SetAutoBrightnessTarget',-1.0)]:
            try:request(method,value)
            except Exception as error:cleanup.append(str(error))
        try:
            result['principal_after']=principal(bus,args.shell_executable)
            if result['principal_after']!=identity:raise RuntimeError('original final shell/compositor principal changed')
        except Exception as error:cleanup.append(str(error))
        if signal_overflow[0]:cleanup.append('original signal receipt overflow')
        result['signal_receipts']=signals;result['signal_overflow']=signal_overflow[0]
        result['cleanup_errors']=cleanup
        bus.signal_unsubscribe(subscription)
        try:retain(args.out,result)
        except Exception as error:
            if primary:print('secondary receipt error: '+str(error),file=__import__('sys').stderr)
            else:raise
    if primary:raise primary
    if cleanup:raise RuntimeError('brightness proof reset failed: '+'; '.join(cleanup))
    print(json.dumps(result))

if __name__=='__main__':main()
