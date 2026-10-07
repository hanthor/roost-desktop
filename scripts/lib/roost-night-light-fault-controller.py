#!/usr/bin/python3
"""Bounded root-only handoff to the separately installed fault fixture helper.

The UID1000 public snapshot is attributable only under this isolated fixture's
same-UID assumption. Its original semantic transform must match the independently
retained warm-pixel observation within five seconds; each FD receipt is retained
separately when the actual snapshot counters advance. The snapshot is not a producer authentication primitive.
"""
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import time

OUT = Path('/out')
stop = False
owned_marker = None


def terminate(_number, _frame):
    global stop
    stop = True


def read_request():
    path = OUT / 'fault-request.json'
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_uid != 1000 or not 0 < before.st_size <= 8192:
            raise RuntimeError('invalid bounded original ordinary-user request')
        raw = stream.read(8193)
        after = os.fstat(stream.fileno())
    named = path.lstat()
    key = lambda row: (row.st_dev, row.st_ino, row.st_uid, row.st_mode, row.st_size, row.st_mtime_ns, row.st_ctime_ns)
    if len(raw) > 8192 or key(before) != key(after) or key(after) != key(named):
        raise RuntimeError('request changed during admission')
    request = json.loads(raw)
    if set(request) != {'pid', 'start', 'exe_sha256', 'baseline_receipt', 'baseline_transform'}:
        raise RuntimeError('unknown request fields')
    if type(request['pid']) is not int or request['pid'] <= 1 or type(request['start']) is not int or request['start'] <= 0:
        raise RuntimeError('invalid original process identity')
    receipt = request['baseline_receipt']
    if set(receipt) != {'sha256', 'dev', 'inode', 'size', 'mtime_ns', 'ctime_ns', 'observed_wall_ns'}:
        raise RuntimeError('invalid independent original snapshot receipt')
    return request


def compare_admitted(request, receipt):
    if receipt['transform'] != request['baseline_transform']:
        raise RuntimeError('admitted original transform differs from independent warm proof')
    elapsed = receipt['admitted_wall_ns'] - request['baseline_receipt']['observed_wall_ns']
    if not 0 <= elapsed <= 5_000_000_000:
        raise RuntimeError('warm proof admission clock interval exceeded or reversed')
    return {'baseline_receipt':request['baseline_receipt'], 'admitted_receipt':receipt,
            'capture_to_admission_ns':elapsed, 'same_snapshot_required':False}


def publish(name, value):
    path = OUT / name
    raw = json.dumps(value, indent=2).encode()
    if len(raw) > 16384:
        raise RuntimeError('controller result bound')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(raw)


try:
    if os.getuid() != 0:
        raise RuntimeError('root controller required')
    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGINT, terminate)
    end = time.monotonic() + 300
    while not stop and time.monotonic() < end and not (OUT / 'fault-request.json').exists():
        time.sleep(.05)
    if stop or time.monotonic() >= end:
        raise RuntimeError('no bounded genuine warm request')
    request = read_request()
    result = subprocess.run([
        '/repo/packaging/marlin/vm-lane/roost-vm-night-light-fault',
        str(request['pid']), str(request['start']), request['exe_sha256'],
        '/out/compositor-state.json', '/out/fault-request.json'], capture_output=True, timeout=15)
    if len(result.stdout) > 16384 or len(result.stderr) > 8192:
        raise RuntimeError('fault helper output bound')
    (OUT / 'fault-helper-raw.json').write_bytes(result.stdout)
    (OUT / 'fault-helper-stderr.txt').write_bytes(result.stderr)
    receipt = json.loads(result.stdout)
    if result.returncode != 0:
        raise RuntimeError('original strict fault helper rejected admission')
    owned_marker = receipt['marker']
    receipt['independent_warm_comparison'] = compare_admitted(request, receipt['state_receipt'])
    if not receipt['original_state_route_matched'] or receipt['controller_uid'] != 0:
        raise RuntimeError('original route or controller identity mismatch')
    publish('fault-controller-result.json', receipt)
    while not stop and time.monotonic() < end:
        time.sleep(.05)
except BaseException as error:
    if not (OUT / 'fault-controller-result.json').exists():
        publish('fault-controller-result.json', {'failure_type': type(error).__name__, 'message': str(error)[:1024]})
finally:
    cleanup = {'owned_marker_present': owned_marker is not None, 'removed': False}
    if owned_marker is not None:
        try:
            expected = f"/run/roost-vm-night-light-fault-{request['pid']}"
            if owned_marker['path'] != expected:
                raise RuntimeError('unexpected helper marker route')
            path = Path(expected)
            named = path.lstat()
            if (named.st_dev, named.st_ino, named.st_uid) != (owned_marker['dev'], owned_marker['inode'], 0):
                raise RuntimeError('owned marker replaced before cleanup')
            path.unlink()
            cleanup['removed'] = True
        except Exception as error:
            cleanup['failure_type'] = type(error).__name__
    publish('fault-marker-cleanup.json', cleanup)
