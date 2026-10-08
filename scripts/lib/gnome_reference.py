"""Finite CI benchmark reference receipts; unavailable tracing is never zero."""
import math
import json
import os
import stat
import re

PROFILES = ('diagnostic', 'stock')


def validate_image(value, profile):
    if profile not in PROFILES or type(value) is not dict or set(value) != {
            'schema', 'profile', 'mutter_package', 'shell_package', 'packages_sha256', 'critical_sha256'}:
        raise ValueError('reference image schema refused')
    if type(value['schema']) is not int or value['schema'] != 1 or value['profile'] != profile:
        raise ValueError('reference image profile mismatch')
    if not isinstance(value['mutter_package'], str) or not re.fullmatch(r'mutter 51\.0-[A-Za-z0-9.+_]+', value['mutter_package']):
        raise ValueError('reference Mutter package refused')
    if not isinstance(value['shell_package'], str) or not re.fullmatch(r'gnome-shell (?:[0-9]+:)?51\.0-[A-Za-z0-9.+_]+', value['shell_package']):
        raise ValueError('reference Shell package refused')
    if profile == 'diagnostic' and value['mutter_package'] != 'mutter 51.0-1.7':
        raise ValueError('diagnostic package mismatch')
    for key in ('packages_sha256', 'critical_sha256'):
        if not isinstance(value[key], str) or not re.fullmatch('[0-9a-f]{64}', value[key]):
            raise ValueError('reference manifest digest refused')
    return value


def validate_reference(value, profile):
    if type(value) is not dict or set(value) != {'image', 'principal', 'shell_version', 'shell_sha256', 'modules'}:
        raise ValueError('reference process schema refused')
    validate_image(value['image'], profile)
    principal = value['principal']
    if type(principal) is not dict or set(principal) != {'uid', 'pid', 'start', 'owner'}:
        raise ValueError('reference principal schema refused')
    for key in ('uid', 'pid', 'start'):
        if type(principal[key]) is not int or not 0 < principal[key] < 2**64:
            raise ValueError('reference principal integer refused')
    if not isinstance(principal['owner'], str) or not re.fullmatch(r':[0-9]+\.[0-9]+', principal['owner']):
        raise ValueError('reference unique owner refused')
    if value['shell_version'] != 'GNOME Shell 51.0' or not isinstance(value['shell_sha256'], str) or not re.fullmatch('[0-9a-f]{64}', value['shell_sha256']):
        raise ValueError('reference Shell identity refused')
    modules = value['modules']
    if type(modules) is not dict or set(modules) != {'core', 'clutter', 'cogl'}:
        raise ValueError('reference module set refused')
    for module in modules.values():
        if type(module) is not dict or set(module) != {'path', 'device', 'inode', 'uid', 'bytes', 'sha256', 'package'}:
            raise ValueError('reference module schema refused')
        if not isinstance(module['path'], str) or not module['path'].startswith('/usr/lib/') or len(module['path']) > 4096:
            raise ValueError('reference module path refused')
        if type(module['uid']) is not int or module['uid'] != 0 or module['package'] != value['image']['mutter_package']:
            raise ValueError('reference module authority refused')
        for key in ('device', 'inode', 'bytes'):
            if type(module[key]) is not int or not 0 < module[key] < 2**64:
                raise ValueError('reference module identity refused')
        if module['bytes'] > 16 * 1024 * 1024 or not isinstance(module['sha256'], str) or not re.fullmatch('[0-9a-f]{64}', module['sha256']):
            raise ValueError('reference module bytes refused')
    return value


def same_reference(before, after, profile):
    validate_reference(before, profile)
    validate_reference(after, profile)
    if before != after:
        raise ValueError('original reference process or modules changed')


def trace_required(desktop, profile):
    if desktop not in ('gnome', 'roost') or profile not in PROFILES:
        raise ValueError('unknown benchmark profile')
    return desktop == 'gnome' and profile == 'diagnostic'


def idle_memory(samples, start, end, field):
    if field not in ('pss_kib', 'rss_kib'):
        raise ValueError('unknown phase memory field')
    if not all(type(value) in (int, float) and math.isfinite(value) for value in (start, end)) or not 0 < start < end:
        raise ValueError('invalid guest idle interval')
    values = []
    for sample in samples:
        begin = sample.get('cpu_interval_start_boottime_s')
        finish = sample.get('boottime_s')
        # The previous sample's guest-boottime start precedes the current
        # collection. This conservative, wholly idle CPU interval therefore
        # also bounds its memory collection, without mixing monotonic duration
        # with boottime or introducing a synthetic collection start timestamp.
        if type(finish) not in (int, float) or not math.isfinite(finish):
            raise ValueError('invalid resource collection clock')
        if begin is None:
            continue  # First observation has no previous interval.
        if type(begin) not in (int, float) or not math.isfinite(begin) or begin > finish:
            raise ValueError('invalid resource interval clock')
        if start <= begin <= finish <= end:
            value = sample.get(field)
            if type(value) is not int or value < 0:
                raise ValueError('invalid idle memory sample')
            values.append(value)
    if len(values) < 20:
        raise ValueError('fewer than twenty wholly idle memory collections')
    return values


def idle_pss(samples, start, end):
    return idle_memory(samples, start, end, 'pss_kib')


def read_image(path, profile):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_mode & 0o022 or not 0 < before.st_size <= 4096:
            raise ValueError('host image receipt type/size/mode refused')
        raw = stream.read(4097)
        after = os.fstat(stream.fileno())
        named = os.stat(path, follow_symlinks=False)
        identity = lambda value: (value.st_dev, value.st_ino, value.st_size, value.st_mode, value.st_uid, value.st_mtime_ns, value.st_ctime_ns)
        if len(raw) != before.st_size or identity(before) != identity(after) or identity(after) != identity(named):
            raise ValueError('host image receipt changed')
    return validate_image(json.loads(raw), profile)
