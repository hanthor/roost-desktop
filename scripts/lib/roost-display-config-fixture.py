#!/usr/bin/env python3
"""One-shot real DisplayConfig calls from the original nested proof principal.

Controlled same-user fixture provenance, not an authentication boundary. Gio
method budgets do not bound bus setup or kernel file operations. No retries.
"""
import json
import hashlib
import math
import os
from pathlib import Path
import stat
import sys
import time

NAME = 'org.gnome.Mutter.DisplayConfig'
PATH = '/org/gnome/Mutter/DisplayConfig'
STATE_TYPE = '(ua((ssss)a(siiddada{sv})a{sv})a(iiduba(ssss)a{sv})a{sv})'
APPLY_TYPE = '(uua(iiduba(ssa{sv}))a{sv})'


def key(value):
    return [value.st_dev, value.st_ino, value.st_uid, value.st_mode,
            value.st_size, value.st_mtime_ns, value.st_ctime_ns]


def bounded(path, limit):
    with open(path, 'rb') as source:
        value = source.read(limit + 1)
    if len(value) > limit:
        raise ValueError('bound')
    return value


def process(pid, executable, socket):
    uid = os.getuid()
    if type(pid) is not int or not 0 < pid < 2**32 or uid == 0 or uid != os.geteuid():
        raise ValueError('principal')
    proc = Path(f'/proc/{pid}')
    named = executable.lstat()
    if not stat.S_ISREG(named.st_mode) or named.st_uid not in (0, uid):
        raise ValueError('executable')
    with os.fdopen(os.open(executable, os.O_RDONLY | os.O_NOFOLLOW), 'rb') as source:
        original = key(os.fstat(source.fileno()))
        if original != key(named) or key((proc/'exe').stat()) != original:
            raise ValueError('executable')
        if os.readlink(proc/'exe') != str(executable) or proc.stat().st_uid != uid:
            raise ValueError('principal')
        ticks = int(bounded(proc/'stat', 4096).decode('utf-8').rsplit(')', 1)[1].split()[19])
        # Environment values stay in bounded scratch memory; receipts keep only
        # equality facts. These were explicitly supplied by the owning fixture.
        environment = bounded(proc/'environ', 1024 * 1024).split(b'\0')
        for name in ('DBUS_SESSION_BUS_ADDRESS', 'ROOST_COMPOSITOR_STATE'):
            expected = os.environ.get(name)
            if not expected or environment.count(os.fsencode(name+'='+expected)) != 1:
                raise ValueError('route')
        runtime = Path(os.environ['XDG_RUNTIME_DIR'])
        directory = runtime.lstat()
        if not stat.S_ISDIR(directory.st_mode) or directory.st_uid != uid or directory.st_mode & 0o077:
            raise ValueError('route')
        if not socket or Path(socket).name != socket or socket in ('.', '..'):
            raise ValueError('route')
        endpoint = (runtime/socket).lstat()
        if not stat.S_ISSOCK(endpoint.st_mode) or endpoint.st_uid != uid:
            raise ValueError('route')
        if (ticks <= 0 or key(os.fstat(source.fileno())) != original
                or key(executable.lstat()) != original or key((proc/'exe').stat()) != original
                or proc.stat().st_uid != uid or key((runtime/socket).lstat()) != key(endpoint)
                or int(bounded(proc/'stat', 4096).decode('utf-8').rsplit(')', 1)[1].split()[19]) != ticks):
            raise ValueError('principal')
    return {'pid':pid,'uid':uid,'start_ticks':ticks,'executed_key':original,
            'socket_key':key(endpoint),'session_bus_matches':True,'state_route_matches':True}


def credentials(call):
    owner, = call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                  'GetNameOwner', '(s)', (NAME,), '(s)', 500)
    if type(owner) is not str or len(owner)>128 or not owner.startswith(':'):
        raise ValueError('owner')
    pid, = call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                'GetConnectionUnixProcessID', '(s)', (owner,), '(u)', 500)
    uid, = call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                'GetConnectionUnixUser', '(s)', (owner,), '(u)', 500)
    if type(pid) is not int or pid <= 0 or type(uid) is not int or not 0 < uid < 2**32:
        raise ValueError('credentials')
    return {'owner':owner,'pid':pid,'uid':uid}


def admitted(original, actual, bus):
    if actual != original['principal'] or bus != original['bus']:
        raise ValueError('replacement')
    if bus['pid'] != actual['pid'] or bus['uid'] != actual['uid']:
        raise ValueError('credentials')


def current_serial(value, width, height):
    if type(value) not in (tuple,list) or len(value)!=4:
        raise ValueError('state')
    serial, monitors, logical, properties = value
    if type(serial) is not int or not 0 < serial < 2**32:
        raise ValueError('serial')
    if type(monitors) not in (tuple,list) or len(monitors)!=1 or type(logical) not in (tuple,list) or len(logical)!=1 or type(properties) is not dict:
        raise ValueError('state')
    monitor=monitors[0]
    if type(monitor) not in (tuple,list) or len(monitor)!=3:
        raise ValueError('monitor')
    names,modes,_=monitor
    if type(names) not in (tuple,list) or len(names)!=4 or any(type(x) is not str or len(x)>256 for x in names) or names[0]!='roost-0':
        raise ValueError('connector')
    if type(modes) not in (tuple,list) or len(modes)!=1:
        raise ValueError('mode')
    mode=modes[0]
    if (type(mode) not in (tuple,list) or len(mode)!=7 or mode[0]!=f'{width}x{height}@60.000'
            or type(mode[1]) is not int or type(mode[2]) is not int or mode[1:3] not in ([width,height],(width,height))
            or type(mode[3]) not in (float,int) or mode[3]!=60.0):
        raise ValueError('mode')
    item=logical[0]
    if (type(item) not in (tuple,list) or len(item)!=7 or type(item[0]) is not int or item[0]!=0
            or type(item[1]) is not int or item[1]!=0 or type(item[2]) not in (float,int)
            or not math.isfinite(item[2]) or item[2] not in (1.0,1.25)
            or type(item[3]) is not int or item[3]!=0 or item[4] is not True
            or type(item[5]) not in (tuple,list) or len(item[5])!=1 or list(item[5][0])!=list(names)):
        raise ValueError('geometry')
    physical = hashlib.sha256(json.dumps([list(names), list(mode[:4])], separators=(',',':')).encode('utf-8')).hexdigest()
    return serial, physical


def write_original(path, value):
    data=json.dumps(value,sort_keys=True).encode('ascii')
    if len(data)>4096:raise ValueError('bound')
    with os.fdopen(os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600),'wb') as output:
        output.write(data);output.flush();os.fsync(output.fileno())


def read_original(path):
    with os.fdopen(os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK),'rb') as source:
        before=os.fstat(source.fileno());stamp=key(before)
        if not stat.S_ISREG(before.st_mode) or before.st_uid!=os.getuid() or before.st_nlink!=1 or stat.S_IMODE(before.st_mode)!=0o600 or not 0<before.st_size<=4096:
            raise ValueError('receipt')
        raw=source.read(4097)
        if len(raw)!=before.st_size or key(os.fstat(source.fileno()))!=stamp or key(path.lstat())!=stamp:
            raise ValueError('receipt')
    value=json.loads(raw)
    if (type(value) is not dict or set(value)!={'schema','principal','bus','physical_output_sha256'} or type(value['schema']) is not int or value['schema']!=1
            or type(value['physical_output_sha256']) is not str or len(value['physical_output_sha256'])!=64):
        raise ValueError('receipt')
    principal=value['principal'];bus=value['bus']
    if (type(principal) is not dict or set(principal)!={'pid','uid','start_ticks','executed_key','socket_key','session_bus_matches','state_route_matches'}
            or type(bus) is not dict or set(bus)!={'owner','pid','uid'}):raise ValueError('receipt')
    for field,limit in [('pid',2**32),('uid',2**32),('start_ticks',2**64)]:
        if type(principal[field]) is not int or not 0<principal[field]<limit:raise ValueError('receipt')
    for field in ('executed_key','socket_key'):
        if type(principal[field]) is not list or len(principal[field])!=7 or any(type(x) is not int or not 0<=x<2**64 for x in principal[field]):raise ValueError('receipt')
    if (principal['session_bus_matches'] is not True or principal['state_route_matches'] is not True
            or type(bus['pid']) is not int or bus['pid']!=principal['pid']
            or type(bus['uid']) is not int or bus['uid']!=principal['uid']
            or type(bus['owner']) is not str or len(bus['owner'])>128 or not bus['owner'].startswith(':')):raise ValueError('receipt')
    return value


def apply(original, observe, call, width, height, method, scale):
    if type(method) is not int or method not in (1,2) or type(scale) is not float or scale not in (1.0,1.25):
        raise ValueError('request')
    admitted(original,observe(),credentials(call))
    state=call(original['bus']['owner'],PATH,NAME,'GetCurrentState',None,None,STATE_TYPE,500)
    serial,physical=current_serial(state,width,height)
    if physical!=original['physical_output_sha256']:raise ValueError('physical-output')
    admitted(original,observe(),credentials(call))
    call(original['bus']['owner'],PATH,NAME,'ApplyMonitorsConfig',APPLY_TYPE,
         (serial,method,[(0,0,scale,0,True,[('roost-0',f'{width}x{height}@60.000',{})])],{}),'()',3000)
    admitted(original,observe(),credentials(call))
    return {'schema':1,'serial':serial,'method':method,'scale':scale,'original_principal_preserved':True,'original_bus_owner_preserved':True}


def main(argv):
    stage='arguments'
    try:
        if len(argv) not in (8,10) or argv[0] not in ('pin','apply'):
            raise ValueError('arguments')
        command,pid,executable,socket,receipt=argv[:5]
        if (command=='pin' and len(argv)!=8) or (command=='apply' and len(argv)!=10):raise ValueError('arguments')
        pid=int(pid);executable=Path(executable).resolve(strict=True);receipt=Path(receipt)
        # Fixed command sentinel avoids ambiguous positional interpretation.
        if argv[5]!='--':raise ValueError('arguments')
        from gi.repository import Gio,GLib
        stage='bus-setup';bus=Gio.bus_get_sync(Gio.BusType.SESSION,None)
        deadline=time.monotonic()+8
        def call(dest,path,interface,method,signature,values,reply_type,budget):
            remaining=deadline-time.monotonic()
            if remaining<=0:raise TimeoutError('deadline')
            result=bus.call_sync(dest,path,interface,method,GLib.Variant(signature,values) if signature else None,
                GLib.VariantType.new(reply_type),Gio.DBusCallFlags.NO_AUTO_START,min(budget,max(1,int(remaining*1000))),None)
            if time.monotonic()>deadline:raise TimeoutError('deadline')
            return result.unpack()
        observe=lambda:process(pid,executable,socket)
        stage='original-authority'
        if command=='pin':
            principal=observe();original={'schema':1,'principal':principal,'bus':credentials(call)}
            admitted(original,observe(),credentials(call))
            width,height=int(argv[6]),int(argv[7])
            if not 1<=width<=16384 or not 1<=height<=16384:raise ValueError('geometry')
            state=call(original['bus']['owner'],PATH,NAME,'GetCurrentState',None,None,STATE_TYPE,500)
            _,original['physical_output_sha256']=current_serial(state,width,height)
            admitted(original,observe(),credentials(call));write_original(receipt,original)
        else:
            original=read_original(receipt);width,height,method=int(argv[6]),int(argv[7]),int(argv[8]);scale=float(argv[9])
            if not 1<=width<=16384 or not 1<=height<=16384:raise ValueError('geometry')
            stage='display-apply'
            print(json.dumps(apply(original,observe,call,width,height,method,scale),sort_keys=True))
    except Exception as error:
        kind='timeout' if isinstance(error,TimeoutError) else 'os-error' if isinstance(error,OSError) else 'rejected'
        print(json.dumps({'stage':stage,'error':kind}),file=sys.stderr)
        raise SystemExit(1) from None


if __name__=='__main__':main(sys.argv[1:])
