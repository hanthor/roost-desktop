#!/usr/bin/python3
"""Fixed genuine Color/Screenshot readiness, not ScreenCast capabilities.

GNOME GSD51 plugins/color/gsd-color-manager.c exports Color at /Color with
Temperature/active/temporary-disable. GNOME portal51 screenshot.c and portal
1.20 Screenshot XML expose Screenshot version2, without ScreenCast bitfields.
Readiness is admission only; the journey still proves actual warm rendering.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import time

SPECS = {
    'color': ('org.gnome.SettingsDaemon.Color', '/org/gnome/SettingsDaemon/Color',
              'org.gnome.SettingsDaemon.Color', '/usr/libexec/gsd-color', 'gnome-settings-daemon'),
    'backend': ('org.freedesktop.impl.portal.desktop.gnome', '/org/freedesktop/portal/desktop',
                'org.freedesktop.impl.portal.Screenshot', '/usr/libexec/xdg-desktop-portal-gnome', 'xdg-desktop-portal-gnome'),
    'frontend': ('org.freedesktop.portal.Desktop', '/org/freedesktop/portal/desktop',
                 'org.freedesktop.portal.Screenshot', '/usr/libexec/xdg-desktop-portal', 'xdg-desktop-portal'),
}


def resource_key(info):
    return (info.st_dev, info.st_ino, info.st_uid, info.st_mode, info.st_size,
            info.st_mtime_ns, info.st_ctime_ns)


def bounded(path, limit, proc=False, expected=None):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit or (not proc and before.st_size <= 0):
            raise RuntimeError('bounded resource required')
        if expected is not None and resource_key(before) != expected:
            raise RuntimeError('original executable hash FD changed')
        body = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
    if (len(body) > limit or resource_key(before) != resource_key(after) or
            resource_key(after) != resource_key(os.stat(path, follow_symlinks=False))):
        raise RuntimeError('original resource changed')
    return body


def process(pid, start, exe):
    if type(pid) is not int or pid <= 0 or type(start) is not int or start <= 0:
        raise RuntimeError('original process arguments required')
    root = Path('/proc') / str(pid)
    first = bounded(root / 'stat', 8192, proc=True)
    ticks = int(first.decode().rsplit(')', 1)[1].split()[19])
    status = bounded(root / 'status', 65536, proc=True).decode().splitlines()
    uids = [int(v) for v in next(line for line in status if line.startswith('Uid:')).split()[1:]]
    metadata = os.stat(exe, follow_symlinks=False)
    mapped = os.stat(root / 'exe')
    if (ticks != start or uids != [1000] * 4 or os.readlink(root / 'exe') != exe or
            metadata.st_uid != 0 or metadata.st_mode & 0o022 or not stat.S_ISREG(metadata.st_mode) or
            resource_key(mapped) != resource_key(metadata)):
        raise RuntimeError('original ordinary-user installed process required')
    digest = hashlib.sha256(bounded(exe, 128 * 1024 * 1024, expected=resource_key(mapped))).hexdigest()
    last_ticks = int(bounded(root / 'stat', 8192, proc=True).decode().rsplit(')', 1)[1].split()[19])
    last_status = bounded(root / 'status', 65536, proc=True).decode().splitlines()
    last_uids = [int(v) for v in next(line for line in last_status if line.startswith('Uid:')).split()[1:]]
    last_mapped = os.stat(root / 'exe')
    last_named = os.stat(exe, follow_symlinks=False)
    if (last_ticks != ticks or last_uids != uids or os.readlink(root / 'exe') != exe or
            resource_key(last_mapped) != resource_key(mapped) or
            resource_key(last_named) != resource_key(metadata)):
        raise RuntimeError('original process replaced during identity read')
    return {'pid': pid, 'uid': 1000, 'start': ticks, 'exe': exe, 'sha256': digest,
            'exe_dev': mapped.st_dev, 'exe_inode': mapped.st_ino}


def command(args):
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        with subprocess.Popen(args, stdout=out, stderr=err) as child:
            try:
                code = child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill(); child.wait(timeout=5)
                raise
        if code or out.tell() > 4096 or err.tell():
            raise RuntimeError('genuine package command failed')
        out.seek(0)
        return out.read(4096).decode().strip()


def package(kind):
    _name, _path, _interface, exe, expected = SPECS[kind]
    if command(['/usr/bin/rpm', '-qf', '--queryformat', '%{NAME}\n', exe]) != expected:
        raise RuntimeError('genuine package owner required')
    version = command(['/usr/bin/rpm', '-q', '--queryformat', '%{VERSION}\n', expected])
    if kind != 'frontend' and version.split('.')[0] != '51':
        raise RuntimeError('genuine GNOME51 package required')
    if command(['/usr/bin/rpm', '-Vf', exe]):
        raise RuntimeError('genuine package verification required')
    return {'name': expected, 'version': version, 'verified': True}


def properties(kind, values):
    if type(values) is not dict:
        raise RuntimeError('typed readiness properties required')
    if kind == 'color':
        required = ('Temperature', 'NightLightActive', 'DisabledUntilTomorrow')
        if (any(k not in values for k in required) or type(values['Temperature']) is not int or
                not 1000 <= values['Temperature'] <= 10000 or type(values['NightLightActive']) is not bool or
                type(values['DisabledUntilTomorrow']) is not bool):
            raise RuntimeError('genuine Color property contract required')
        return {k: values[k] for k in required}
    value = values.get('version')
    if type(value) is not int or not 2 <= value <= 0xffffffff:
        raise RuntimeError('genuine Screenshot version2 required')
    return {'version': value}


def probe(kind, original, bus_property, get_properties, read_process=process):
    name, path, interface, exe, _package = SPECS[kind]
    owner = bus_property('GetNameOwner', name)
    if type(owner) is not str or not owner.startswith(':') or len(owner) > 128:
        raise RuntimeError('original unique owner required')
    pid = bus_property('GetConnectionUnixProcessID', owner)
    uid = bus_property('GetConnectionUnixUser', owner)
    if type(pid) is not int or type(uid) is not int or pid != original['pid'] or uid != 1000:
        raise RuntimeError('original broker principal required')
    if read_process(pid, original['start'], exe) != original:
        raise RuntimeError('original process changed before property read')
    values = properties(kind, get_properties(owner, path, interface))
    if (bus_property('GetNameOwner', name) != owner or
            bus_property('GetConnectionUnixProcessID', owner) != pid or
            bus_property('GetConnectionUnixUser', owner) != uid or
            read_process(pid, original['start'], exe) != original):
        raise RuntimeError('original owner changed during property read')
    return {'owner': owner, 'process': original, 'properties': values,
            'scope': 'genuine readonly property readiness; actual rendering/capture pending'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('kind', choices=tuple(SPECS))
    parser.add_argument('output')
    parser.add_argument('--pid', type=int, required=True)
    parser.add_argument('--start', type=int, required=True)
    args = parser.parse_args()
    if os.getuid() != 1000:
        raise RuntimeError('ordinary UID1000 readiness session required')
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    exe = SPECS[args.kind][3]
    original = process(args.pid, args.start, exe)
    authority = package(args.kind)
    deadline = time.monotonic() + 30
    def check():
        if time.monotonic() >= deadline:
            raise RuntimeError('bounded readiness deadline')
    def call(owner, path, interface, method, signature, values):
        check()
        reply_type = '(a{sv})' if method == 'GetAll' else ('(s)' if method == 'GetNameOwner' else '(u)')
        result = bus.call_sync(owner, path, interface, method, GLib.Variant(signature, values), GLib.VariantType.new(reply_type),
                               Gio.DBusCallFlags.NO_AUTO_START, 500, None).unpack()[0]
        check()
        return result
    def bus_property(method, name):
        return call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', method, '(s)', (name,))
    def get_properties(owner, path, interface):
        return call(owner, path, 'org.freedesktop.DBus.Properties', 'GetAll', '(s)', (interface,))
    while time.monotonic() < deadline:
        try:
            result = probe(args.kind, original, bus_property, get_properties)
            check()
            if package(args.kind) != authority or process(args.pid, args.start, exe) != original:
                raise RuntimeError('original readiness authority changed')
            # Package verification itself spans several bounded calls. Recheck
            # original broker ownership after it, before saving admission.
            if (bus_property('GetNameOwner', SPECS[args.kind][0]) != result['owner'] or
                    bus_property('GetConnectionUnixProcessID', result['owner']) != original['pid'] or
                    bus_property('GetConnectionUnixUser', result['owner']) != original['uid'] or
                    process(args.pid, args.start, exe) != original):
                raise RuntimeError('original readiness owner changed at completion')
            check()
            result['package'] = authority
            Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
            return
        except GLib.Error:
            # Name/object startup can be asynchronous. No error body is saved,
            # replacement principals and malformed properties fail immediately.
            if process(args.pid, args.start, exe) != original:
                raise RuntimeError('original startup process changed')
            time.sleep(.1)
    raise RuntimeError('genuine readiness properties did not become available')


if __name__ == '__main__':
    main()
