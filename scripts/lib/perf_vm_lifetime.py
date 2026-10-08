"""Bounded host-owned child departure handoff for paired CI acquisitions.

These same-user receipts record controlled source provenance, not an unforgeable
security boundary or successful guest measurement. Kernel wait/pidfd establish
original child departure; the original metric/package/guest gates remain separate.
"""
import json
import os
from pathlib import Path
import select
import stat
import subprocess

LIMIT = 4096
PROFILES = {'diagnostic', 'stock'}
ORDER = (('gnome', 1), ('roost', 1), ('roost', 2), ('gnome', 2), ('gnome', 3), ('roost', 3))
STAGES = {'setup', 'image-build', 'image-probe', 'disk-create', 'disk-install', 'disk-owner',
          'measurement', 'disk-remove', 'image-remove', 'complete'}


def metadata(s):
    return (s.st_dev, s.st_ino, s.st_uid, s.st_mode, s.st_nlink, s.st_size, s.st_mtime_ns, s.st_ctime_ns)


def identity(s):
    return (s.st_dev, s.st_ino, s.st_uid, s.st_mode, s.st_nlink)


def process(pid):
    if type(pid) is not int or pid <= 0:
        raise RuntimeError('original host process PID required')
    root = Path('/proc') / str(pid)
    def read(name, limit):
        with os.fdopen(os.open(root / name, os.O_RDONLY | os.O_NOFOLLOW), 'rb') as stream:
            value = stream.read(limit + 1)
        if len(value) > limit:
            raise RuntimeError('original host process metadata exceeds bound')
        return value.decode()
    start = int(read('stat', 8192).rsplit(')', 1)[1].split()[19])
    rows = read('status', 65536).splitlines()
    uids = [int(value) for value in next(row for row in rows if row.startswith('Uid:')).split()[1:]]
    final = int(read('stat', 8192).rsplit(')', 1)[1].split()[19])
    if start <= 0 or final != start or uids != [os.getuid()] * 4:
        raise RuntimeError('original host process identity changed')
    return {'pid': pid, 'uid': os.getuid(), 'start_ticks': start}


def principal(value):
    return (type(value) is dict and set(value) == {'pid', 'uid', 'start_ticks'}
            and type(value['pid']) is int and value['pid'] > 0
            and type(value['uid']) is int and value['uid'] == os.getuid()
            and type(value['start_ticks']) is int and value['start_ticks'] > 0)


def departed(value):
    try:
        current = process(value['pid'])
    except (FileNotFoundError, ProcessLookupError):
        return True
    return current != value


def read_receipt(path, private=False):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if (not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid() or before.st_nlink != 1
                or before.st_mode & 0o022 or (private and stat.S_IMODE(before.st_mode) != 0o600)
                or not 0 < before.st_size <= LIMIT):
            raise RuntimeError('original bounded handoff resource required')
        raw = stream.read(LIMIT + 1)
        after = os.fstat(stream.fileno())
        if (len(raw) != before.st_size or len(raw) > LIMIT or metadata(before) != metadata(after)
                or metadata(after) != metadata(Path(path).lstat())):
            raise RuntimeError('original handoff resource changed')
    return json.loads(raw)


class Handoff:
    def __init__(self, out, profile, desktop, repeat):
        if profile not in PROFILES or desktop not in {'gnome', 'roost'} or type(repeat) is not int or repeat not in (1, 2, 3):
            raise RuntimeError('fixed acquisition identity required')
        self.path = Path(out) / 'host-vm-lifecycle.json'
        self.fd = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        self.original = identity(os.fstat(self.fd))
        self.child = None
        self.pidfd = None
        try:
            self.value = {'schema': 1, 'profile': profile, 'desktop': desktop, 'repeat': repeat,
                          'state': 'launching', 'worker': process(os.getpid()), 'child': None,
                          'child_returncode': None, 'pidfd_departed': False, 'vm_departure_only': True}
            self.write()
        except BaseException:
            os.close(self.fd)
            raise

    def write(self):
        if identity(os.fstat(self.fd)) != self.original or identity(self.path.lstat()) != self.original:
            raise RuntimeError('original handoff FD replaced')
        raw = json.dumps(self.value, separators=(',', ':')).encode()
        if len(raw) > LIMIT:
            raise RuntimeError('original handoff schema exceeds bound')
        os.lseek(self.fd, 0, os.SEEK_SET)
        view = memoryview(raw)
        while view:
            count = os.write(self.fd, view)
            if count <= 0:
                raise RuntimeError('original handoff write failed')
            view = view[count:]
        os.ftruncate(self.fd, len(raw))
        os.fsync(self.fd)
        if identity(os.fstat(self.fd)) != self.original or identity(self.path.lstat()) != self.original:
            raise RuntimeError('original handoff resource replaced after write')

    def attach(self, child):
        if self.child is not None:
            raise RuntimeError('one original child only')
        # A returned already-reaped Popen cannot establish an original live PID.
        # While this sole owner's child remains unreaped, its PID cannot recycle.
        if child.poll() is not None:
            raise RuntimeError('original live owned child required')
        self.child = child
        self.pidfd = os.pidfd_open(child.pid, 0)
        original = process(child.pid)
        if select.select([self.pidfd], [], [], 0)[0] or process(child.pid) != original:
            raise RuntimeError('original child changed during admission')
        self.value.update(state='running', child=original)
        self.write()

    def reaped(self, child):
        if child is not self.child or self.pidfd is None or not principal(self.value['child']):
            raise RuntimeError('original owned child handoff required')
        code = child.wait(timeout=0)
        if type(code) is not int or not -255 <= code <= 255 or not select.select([self.pidfd], [], [], 0)[0]:
            raise RuntimeError('original child departure was not observed')
        self.value.update(state='reaped', child_returncode=code, pidfd_departed=True)
        self.write()

    def close(self):
        if self.pidfd is not None:
            os.close(self.pidfd)
            self.pidfd = None
        os.close(self.fd)


def cleanup_owned_vm(vm, agent=None, handoff=None):
    """Transport failure cannot bypass own-child termination and actual reap."""
    transport_error = None
    try:
        if agent is not None:
            try:
                agent.close()
            except BaseException as error:
                transport_error = error
    finally:
        vm.terminate()
        try:
            vm.wait(timeout=10)
        except subprocess.TimeoutExpired:
            vm.kill()
            vm.wait()
        if handoff is not None:
            handoff.reaped(vm)
    if transport_error is not None:
        raise transport_error


def case_path(root, index):
    desktop, repeat = ORDER[index - 1]
    return Path(root) / ('' if repeat == 1 else 'repeat-' + str(repeat)) / desktop / 'host-vm-lifecycle.json'


def require_reaped(path, desktop, repeat):
    value = read_receipt(path, private=True)
    expected = {'schema', 'profile', 'desktop', 'repeat', 'state', 'worker', 'child', 'child_returncode', 'pidfd_departed', 'vm_departure_only'}
    if (type(value) is not dict or set(value) != expected or type(value['schema']) is not int or value['schema'] != 1
            or value['profile'] != 'diagnostic' or value['desktop'] != desktop or type(value['repeat']) is not int or value['repeat'] != repeat
            or value['state'] != 'reaped' or value['pidfd_departed'] is not True or value['vm_departure_only'] is not True
            or not principal(value['worker']) or not principal(value['child'])
            or type(value['child_returncode']) is not int or not -255 <= value['child_returncode'] <= 255):
        raise RuntimeError('original terminal diagnostic handoff required')
    if not departed(value['worker']) or not departed(value['child']):
        raise RuntimeError('original acquisition process still exists')
    return value


def require_stock_handoff(root):
    """Fail closed before any stock image/disk operation, without arbitrary kills."""
    root = Path(root)
    status = read_receipt(root / 'acquisition-status.json')
    fields = {'schema', 'profile', 'exit_status', 'case_index', 'repeat', 'desktop', 'stage', 'all_six_commands_succeeded'}
    if (type(status) is not dict or set(status) != fields or type(status['schema']) is not int or status['schema'] != 1
            or status['profile'] != 'diagnostic' or type(status['exit_status']) is not int or not 0 <= status['exit_status'] <= 255
            or type(status['case_index']) is not int or not 0 <= status['case_index'] <= 6
            or type(status['repeat']) is not int or not 0 <= status['repeat'] <= 3
            or type(status['desktop']) is not str or status['desktop'] not in {'none', 'gnome', 'roost'}
            or type(status['stage']) is not str or status['stage'] not in STAGES
            or type(status['all_six_commands_succeeded']) is not bool):
        raise RuntimeError('fixed original diagnostic acquisition status required')
    index = status['case_index']
    success = status['exit_status'] == 0
    if success != status['all_six_commands_succeeded'] or (success and (index != 6 or status['stage'] != 'complete')):
        raise RuntimeError('original diagnostic completion status is inconsistent')
    if index == 0:
        if status['repeat'] != 0 or status['desktop'] != 'none' or status['stage'] != 'setup' or success:
            raise RuntimeError('original pre-acquisition status is ambiguous')
        reached = 0
    else:
        desktop, repeat = ORDER[index - 1]
        if status['desktop'] != desktop or status['repeat'] != repeat:
            raise RuntimeError('original diagnostic case identity changed')
        measured = status['stage'] in {'measurement', 'disk-remove', 'image-remove', 'complete'}
        reached = index if measured else index - 1
    for number, (desktop, repeat) in enumerate(ORDER, 1):
        path = case_path(root, number)
        if number <= reached:
            require_reaped(path, desktop, repeat)
        elif path.exists() or path.is_symlink():
            raise RuntimeError('unexpected later diagnostic child handoff')
    # No success/metric claim: only original guest writers have departed.
    return {'diagnostic_cases_reaped': reached, 'stock_may_start': True, 'metric_qualified': False}
