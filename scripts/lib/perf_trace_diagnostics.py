"""Strict public failure transport for the two fixed GNOME trace controls.

Never serialize exceptions, stderr, arbitrary domain/name strings or user data.
This receipt diagnoses rejection; it cannot qualify a failed acquisition.
"""
import json
import re

MAX_BYTES=4096
ACTIONS={'start','stop'}
STAGES={'gi-import','session-bus','owner-name','owner-pid','owner-uid','peer-executable',
        'peer-validation','shell-version','mutter-provenance','diagnostic-package',
        'capture-open','start-fds','start-call','ownership-marker','cleanup-stop',
        'cleanup-capture','stop-marker-open','stop-marker-validation','stop-owner-validation',
        'stop-version-validation','stop-call','stop-marker-remove','stop-provenance-validation',
        'mutter-map-read','mutter-map-selection','mutter-package-query','mutter-package-validation',
        'mutter-library-open','mutter-library-validation','mutter-library-read','mutter-owner-query',
        'mutter-owner-validation','mutter-map-recheck','diagnostic-transport','failed-capture-acquisition'}
CLASSES={'GLibError','RuntimeError','ValueError','OSError','FileNotFoundError','PermissionError',
         'TimeoutExpired','CalledProcessError','ImportError','ModuleNotFoundError','OtherError'}
DOMAINS={'g-io-error-quark','g-dbus-error-quark','g-file-error-quark','g-spawn-error-quark','other'}
REMOTE={'org.freedesktop.DBus.Error.'+name for name in ('Failed','NoReply','UnknownObject',
        'UnknownMethod','InvalidArgs','AccessDenied','ServiceUnknown','NameHasNoOwner',
        'NotSupported','TimedOut','LimitsExceeded','Disconnected','FileNotFound','IOError')}
REMOTE.add('other')
# Exact static diagnostics from the pinned drained-capture patch only. Never
# retain or summarize arbitrary message text; unknown text stays unclassified.
DRAINED_FAILURES={
    'Capture Start already pending':'start-pending',
    'Invalid drained Start or profiler already running':'invalid-start',
    'Drained Start requires one actual owned stage view':'owned-view',
    'Owned capture stage/view/clock changed':'view-changed',
    'Capture requires one actual Virtual-1 frame clock':'virtual-clock',
    'Main-thread capture tracing failed to enable':'trace-enable',
}

STOP_DRAINED_FAILURES={
    'Owned Stop capture stage/view/clock or tracing changed':'stop-owned-state',
    'Stop capture requires original Virtual-1 frame clock':'stop-clock',
    'No actual Stop drain in 5s':'stop-timeout',
}


def drained_stop_failure(message):
    if type(message) is not str or len(message)>512:return None
    for name in ('Failed','TimedOut'):
        prefix='GDBus.Error:org.freedesktop.DBus.Error.'+name+': '
        if message.startswith(prefix):message=message[len(prefix):];break
    return STOP_DRAINED_FAILURES.get(message)


def drained_failure(message):
    if type(message) is not str or len(message)>512:
        return None
    prefix='GDBus.Error:org.freedesktop.DBus.Error.Failed: '
    if message.startswith(prefix):
        message=message[len(prefix):]
    return DRAINED_FAILURES.get(message)


def validate(value, action):
    if type(action) is not str or action not in ACTIONS or not isinstance(value,dict) or set(value)!={'trace_failure'}:
        raise ValueError('invalid fixed trace failure envelope')
    row=value['trace_failure']
    if not isinstance(row,dict) or set(row)-{'schema','action','stage','error_class','glib','principal','modules','shell_version','component','payload_rejected','subprocess_returncode','drained_failure'}:
        raise ValueError('unknown trace diagnostic fields')
    if row.get('schema')!=1 or type(row.get('schema')) is not int or row.get('action')!=action or type(row.get('stage')) is not str or row['stage'] not in STAGES or type(row.get('error_class')) is not str or row['error_class'] not in CLASSES:
        raise ValueError('invalid trace diagnostic controls')
    if 'payload_rejected' in row or 'subprocess_returncode' in row:
        if (row.get('payload_rejected') is not True or row['stage']!='diagnostic-transport'
                or row['error_class']!='CalledProcessError' or type(row.get('subprocess_returncode')) is not int
                or not -255<=row['subprocess_returncode']<=255 or row['subprocess_returncode']==0):
            raise ValueError('invalid fixed rejected-payload transport')
    if 'component' in row and (type(row['component']) is not str or row['component'] not in {'cogl','clutter','core'}):
        raise ValueError('invalid fixed module component')
    if 'glib' in row:
        item=row['glib']
        if not isinstance(item,dict) or set(item)!={'domain','code','remote_name'} or type(item['domain']) is not str or item['domain'] not in DOMAINS or type(item['remote_name']) is not str or item['remote_name'] not in REMOTE or type(item['code']) is not int or not -(2**31)<=item['code']<2**31:
            raise ValueError('invalid public GLib diagnostic')
    if 'drained_failure' in row:
        label=row['drained_failure']
        start_valid=(action=='start' and row['stage']=='start-call' and label in DRAINED_FAILURES.values()
                     and row.get('glib',{}).get('remote_name')=='org.freedesktop.DBus.Error.Failed')
        stop_valid=(action=='stop' and row['stage']=='stop-call' and label in STOP_DRAINED_FAILURES.values()
                    and row.get('glib',{}).get('remote_name')==
                    ('org.freedesktop.DBus.Error.TimedOut' if label=='stop-timeout' else 'org.freedesktop.DBus.Error.Failed'))
        if (type(label) is not str or row['error_class']!='GLibError'
                or row.get('glib',{}).get('domain')!='g-dbus-error-quark' or not(start_valid or stop_valid)):
            raise ValueError('invalid fixed drained failure classification')
    if 'shell_version' in row and row['shell_version']!='GNOME Shell 51.0':
        raise ValueError('unknown public shell version')
    if 'principal' in row:
        item=row['principal']
        if not isinstance(item,dict) or set(item)!={'uid','pid','owner'} or any(type(item[key]) is not int or not 0<item[key]<2**32 for key in ('uid','pid')) or not isinstance(item['owner'],str) or re.fullmatch(r':[0-9]{1,10}\.[0-9]{1,10}',item['owner']) is None:
            raise ValueError('invalid public original principal')
    if 'modules' in row:
        modules=row['modules']
        if not isinstance(modules,dict) or not modules or set(modules)-{'cogl','clutter','core'}:
            raise ValueError('invalid public module components')
        for item in modules.values():
            if (not isinstance(item,dict) or set(item)!={'sha256','bytes','package'}
                    or not isinstance(item['sha256'],str) or re.fullmatch(r'[0-9a-f]{64}',item['sha256']) is None
                    or type(item['bytes']) is not int or not 0<item['bytes']<=16*1024*1024
                    or item['package']!='mutter 51.0-1.6'):
                raise ValueError('invalid validated public module receipt')
    return value


def decode(raw, action):
    if isinstance(raw,str):raw=raw.encode('utf-8')
    if not isinstance(raw,bytes) or not 0<len(raw)<=MAX_BYTES:
        raise ValueError('bounded trace diagnostic required')
    def unique(pairs):
        value={}
        for key,item in pairs:
            if key in value:raise ValueError('duplicate trace diagnostic field')
            value[key]=item
        return value
    try:
        value=json.loads(raw,object_pairs_hook=unique)
    except RecursionError:
        raise ValueError('trace diagnostic nesting bound') from None
    return validate(value,action)


def encode(value, action):
    raw=json.dumps(validate(value,action),separators=(',',':')).encode()
    if len(raw)>MAX_BYTES:raise ValueError('trace diagnostic output bound')
    return raw


class Context:
    def __init__(self,action):
        if type(action) is not str or action not in ACTIONS:raise ValueError('unsupported diagnostic action')
        self.action=action
        self.stage='gi-import'
        self.public={}
    def failure(self,error,glib_type=None,remote_error=None):
        name=type(error).__name__
        row={'schema':1,'action':self.action,'stage':self.stage,
             'error_class':name if name in CLASSES else 'OtherError',**self.public}
        if glib_type is not None and isinstance(error,glib_type):
            domain=getattr(error,'domain',None)
            code=getattr(error,'code',None)
            remote=remote_error(error) if remote_error is not None else None
            row['error_class']='GLibError'
            if type(code) is int and -(2**31)<=code<2**31:
                row['glib']={'domain':domain if type(domain) is str and domain in DOMAINS else 'other',
                             'code':code,'remote_name':remote if type(remote) is str and remote in REMOTE else 'other'}
                if (self.action=='start' and self.stage=='start-call'
                        and row['glib']['domain']=='g-dbus-error-quark'
                        and row['glib']['remote_name']=='org.freedesktop.DBus.Error.Failed'):
                    known=drained_failure(getattr(error,'message',None))
                    if known is not None:row['drained_failure']=known
                if (self.action=='stop' and self.stage=='stop-call'
                        and row['glib']['domain']=='g-dbus-error-quark'):
                    known=drained_stop_failure(getattr(error,'message',None))
                    expected='org.freedesktop.DBus.Error.TimedOut' if known=='stop-timeout' else 'org.freedesktop.DBus.Error.Failed'
                    if known is not None and row['glib']['remote_name']==expected:row['drained_failure']=known
        return {'trace_failure':row}
