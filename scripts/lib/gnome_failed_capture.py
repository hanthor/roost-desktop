"""Bounded original failed-run acquisition; never a successful Stop receipt."""
import hashlib
import json
import os
import re
import stat
from perf_trace_diagnostics import validate

MAX_RAW=8*1024*1024
MAX_MARKER=8192


def identity(metadata):
    return {'device':metadata.st_dev,'inode':metadata.st_ino,'uid':metadata.st_uid}


def checked_file(metadata,uid):
    if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid!=uid
            or metadata.st_mode & 0o077):
        raise ValueError('failed capture original file ownership refused')


def unique(pairs):
    result={}
    for key,value in pairs:
        if key in result:raise ValueError('duplicate failed source field')
        result[key]=value
    return result


def read_marker(path,uid):
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK|os.O_CLOEXEC)
    try:
        before=os.fstat(fd);checked_file(before,uid)
        if not 0<before.st_size<=MAX_MARKER:raise ValueError('failed capture marker bound')
        raw=b''
        while len(raw)<=MAX_MARKER:
            part=os.read(fd,min(4096,MAX_MARKER+1-len(raw)))
            if not part:break
            raw+=part
        after=os.fstat(fd);named=os.stat(path,follow_symlinks=False)
        checked_file(after,uid);checked_file(named,uid)
        if (identity(before)!=identity(named) or not stat.S_ISREG(named.st_mode)
                or (before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=
                   (after.st_size,after.st_mtime_ns,after.st_ctime_ns)
                or len(raw)!=before.st_size):raise ValueError('failed capture marker changed')
        value=json.loads(raw,object_pairs_hook=unique)
        if type(value) is not dict:raise ValueError('failed capture marker object required')
        return value
    finally:os.close(fd)


def write_marker(path,value,uid):
    raw=json.dumps(value,separators=(',',':')).encode()
    if not 0<len(raw)<=MAX_MARKER:raise ValueError('failed capture marker bound')
    fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW|os.O_CLOEXEC,0o600)
    try:
        before=os.fstat(fd);checked_file(before,uid)
        written=0
        while written<len(raw):
            amount=os.write(fd,raw[written:])
            if amount<=0:raise ValueError('failed capture marker short write')
            written+=amount
        os.fsync(fd)
        after=os.fstat(fd);named=os.stat(path,follow_symlinks=False)
        checked_file(after,uid);checked_file(named,uid)
        if identity(before)!=identity(named) or after.st_size!=len(raw):
            raise ValueError('failed capture marker replacement')
    finally:os.close(fd)


def process_start(pid):
    if type(pid) is not int or pid<=0:raise ValueError('original process required')
    with open(f'/proc/{pid}/stat','rb') as stream:raw=stream.read(8193)
    if len(raw)>8192:raise ValueError('process receipt bound')
    return int(raw.decode().rsplit(')',1)[1].split()[19])


def failed_source(saved,uid,owner,pid,libraries,start):
    if set(saved)!={'source','failure','stop_requested_monotonic_ns'}:
        raise ValueError('unknown failed Stop fields')
    failure=validate(saved['failure'],'stop')['trace_failure']
    if failure.get('drained_failure') not in ('stop-owned-state','stop-clock','stop-timeout'):
        raise ValueError('no original owned Stop cleanup failure')
    source=saved['source']
    keys={'uid','owner','pid','gnome_shell_version','process_start','capture_identity',
          'mutter_cogl_library','mutter_mapped_libraries','start_requested_monotonic_ns',
          'drained_start_required','drained_stop_required','started_monotonic_ns'}
    if (type(source) is not dict or set(source)!=keys
            or any(type(source[key]) is not int or source[key]<=0 for key in
                   ('uid','pid','process_start','start_requested_monotonic_ns','started_monotonic_ns'))
            or type(source['owner']) is not str or re.fullmatch(r':[0-9]{1,10}\.[0-9]{1,10}',source['owner']) is None):
        raise ValueError('failed Stop original source schema refused')
    if (type(source) is not dict or
            (source.get('uid'),source.get('owner'),source.get('pid'))!=(uid,owner,pid)
            or source.get('process_start')!=start
            or failure.get('principal')!={'uid':uid,'pid':pid,'owner':owner}
            or not source['start_requested_monotonic_ns']<=source['started_monotonic_ns']
            or source.get('gnome_shell_version')!='GNOME Shell 51.0'
            or source.get('drained_start_required') is not True
            or source.get('drained_stop_required') is not True
            or set(libraries)!={'cogl','clutter','core'}
            or any(value.get('package')!='mutter 51.0-1.6' for value in libraries.values())
            or source.get('mutter_mapped_libraries')!=libraries
            or source.get('mutter_cogl_library')!=libraries.get('cogl')
            or type(saved['stop_requested_monotonic_ns']) is not int
            or saved['stop_requested_monotonic_ns']<source.get('started_monotonic_ns',0)):
        raise ValueError('failed Stop original source binding refused')
    capture=source.get('capture_identity')
    if (type(capture) is not dict or set(capture)!={'device','inode','uid'}
            or any(type(value) is not int or value<0 for value in capture.values())
            or capture['uid']!=uid or capture['inode']==0):
        raise ValueError('failed Stop original capture identity refused')
    return source


def read_raw(fd,path,original,uid):
    before=os.fstat(fd);checked_file(before,uid)
    if identity(before)!=original or not 256<=before.st_size<=MAX_RAW:
        raise ValueError('failed Stop original raw binding refused')
    raw=b''
    while len(raw)<=MAX_RAW:
        part=os.read(fd,min(65536,MAX_RAW+1-len(raw)))
        if not part:break
        raw+=part
    after=os.fstat(fd);named=os.stat(path,follow_symlinks=False)
    checked_file(after,uid);checked_file(named,uid)
    if (identity(before)!=identity(named) or identity(after)!=original or len(raw)!=before.st_size
            or (before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=
               (after.st_size,after.st_mtime_ns,after.st_ctime_ns)):
        raise ValueError('failed Stop raw capture changed')
    return {'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest(),
            'capture_identity':original,'capture_mtime_ns':after.st_mtime_ns}


SOURCE_KEYS={'uid','owner','pid','gnome_shell_version','process_start','capture_identity',
             'mutter_cogl_library','mutter_mapped_libraries','start_requested_monotonic_ns',
             'drained_start_required','drained_stop_required','started_monotonic_ns'}
RECEIPT_KEYS=SOURCE_KEYS|{'qualified','failure','stop_requested_monotonic_ns',
                         'failed_stop_observed_monotonic_ns','bytes','sha256','capture_mtime_ns',
                         'capture_writers_at_failure','capture_writers_after_failure',
                         'capture_writer_close_wait_ns','writer_closure_observed'}


def validate_receipt(value):
    if type(value) is not dict or set(value)!=RECEIPT_KEYS or value['qualified'] is not False:
        raise ValueError('failed capture finite original receipt required')
    modules=value['mutter_mapped_libraries']
    if type(modules) is not dict or set(modules)!={'cogl','clutter','core'}:
        raise ValueError('failed capture original modules required')
    names={'cogl':'libmutter-cogl-51','clutter':'libmutter-clutter-51','core':'libmutter-51'}
    for component,library in modules.items():
        if (type(library) is not dict or set(library)!={'path','device','inode','uid','bytes','sha256','package'}
                or any(type(library[key]) is not int or library[key]<0 for key in ('device','inode','uid','bytes'))
                or library['uid']!=0 or not 0<library['bytes']<=16*1024*1024 or library['inode']==0
                or library['package']!='mutter 51.0-1.6'
                or type(library['sha256']) is not str or re.fullmatch(r'[0-9a-f]{64}',library['sha256']) is None
                or type(library['path']) is not str or len(library['path'])>255
                or re.fullmatch('/usr/lib/(?:mutter-51/)?'+re.escape(names[component])+r'\.so(?:\.[0-9]+)*',library['path']) is None):
            raise ValueError('failed capture original module receipt refused')
    source={key:value[key] for key in SOURCE_KEYS}
    failed_source({'source':source,'failure':value['failure'],
                   'stop_requested_monotonic_ns':value['stop_requested_monotonic_ns']},
                  value['uid'],value['owner'],value['pid'],modules,value['process_start'])
    if (type(value['bytes']) is not int or not 256<=value['bytes']<=MAX_RAW
            or type(value['sha256']) is not str or re.fullmatch(r'[0-9a-f]{64}',value['sha256']) is None
            or any(type(value[key]) is not int or value[key]<0 for key in
                   ('failed_stop_observed_monotonic_ns','capture_mtime_ns','capture_writer_close_wait_ns'))
            or not value['stop_requested_monotonic_ns']<=value['failed_stop_observed_monotonic_ns']
            or value['writer_closure_observed'] is not True or value['capture_writers_after_failure']!=[]):
        raise ValueError('failed capture original closed raw receipt refused')
    writers=value['capture_writers_at_failure']
    if type(writers) is not list or len(writers)>64:
        raise ValueError('failed capture writer count refused')
    last=-1
    for writer in writers:
        if (type(writer) is not dict or set(writer)!={'fd','flags'}
                or type(writer['fd']) is not int or not last<writer['fd']<2**31
                or type(writer['flags']) is not int or not 0<=writer['flags']<2**32
                or writer['flags'] & os.O_ACCMODE not in (os.O_WRONLY,os.O_RDWR)):
            raise ValueError('failed capture original writer receipt refused')
        last=writer['fd']
    if len(json.dumps(value,separators=(',',':')).encode())>MAX_MARKER:
        raise ValueError('failed capture whole receipt bound')
    return value


def validate_chunk(row,index,metadata):
    if (type(index) is not int or not 0<=index<128
            or type(row) is not dict or set(row)!={'index','data','capture_identity'}
            or type(row['index']) is not int or row['index']!=index
            or type(row['capture_identity']) is not dict
            or set(row['capture_identity'])!={'device','inode','uid'}
            or any(type(value) is not int for value in row['capture_identity'].values())
            or row['capture_identity']!=metadata['capture_identity']
            or type(row['data']) is not str or len(row['data'])>((65536+2)//3)*4):
        raise ValueError('failed capture original chunk schema refused')
    import base64
    raw=base64.b64decode(row['data'],validate=True)
    if len(raw)!=min(65536,metadata['bytes']-index*65536):
        raise ValueError('failed capture original chunk size refused')
    return raw
