#!/usr/bin/env python3
"""Fixed installed Roost session provenance; no commands supplied by callers."""
import hashlib
import json
import os
from pathlib import Path
import socket
import stat
import struct
import subprocess
import sys
import tempfile

BINS = ('roost-compositor', 'roost-session', 'roost-shell-gtk', 'roost-shell-host', 'roost-ibus-bridge', 'roost-greeter')
# Distro LLVM exceeds 128 MiB (libLLVM.so.20.1 is 143,545,784 bytes and is
# mapped by the GTK shell via Mesa); hashing streams in 64 KiB blocks so the
# cap bounds proof time, not memory.
MAX_FILE = 256 * 1024 * 1024


def run(args):
    # Fixed tools can fail noisily: retain only bounded public results, no raw
    # failure body. Temporary files prevent communicate() from allocating it.
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        with subprocess.Popen(args, stdout=stdout, stderr=stderr) as child:
            try:
                status = child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
                raise
        if stdout.tell() > 65536 or stderr.tell() > 65536:
            raise ValueError('command-output-limit')
        if status:
            raise subprocess.CalledProcessError(status, args)
        stdout.seek(0)
        return stdout.read(65536).decode('utf-8').strip()


def digest(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_uid != 0 or before.st_mode & 0o022 or not 0 < before.st_size <= MAX_FILE:
            raise ValueError('immutable-file-required:' + path + ':uid=' + str(before.st_uid)
                             + ':mode=' + oct(before.st_mode & 0o7777) + ':size=' + str(before.st_size))
        h = hashlib.sha256()
        count = 0
        while True:
            block = os.read(fd, min(65536, MAX_FILE + 1 - count))
            if not block:
                break
            count += len(block)
            if count > MAX_FILE:
                raise ValueError('file-limit')
            h.update(block)
        after = os.fstat(fd)
        fields = lambda s: (s.st_dev, s.st_ino, s.st_uid, s.st_mode, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
        if fields(before) != fields(after) or fields(after) != fields(os.stat(path, follow_symlinks=False)) or count != after.st_size:
            raise ValueError('file-changed')
        return {'sha256': h.hexdigest(), 'dev': after.st_dev, 'ino': after.st_ino, 'size': count}
    finally:
        os.close(fd)


def owner(path):
    # usrmerge alternatives are admitted only if they name the same inode.
    alternatives = [path]
    if path.startswith('/usr/lib/'):
        alternative = path[4:]
        if os.path.exists(alternative) and os.path.samefile(path, alternative):
            alternatives.append(alternative)
    for candidate in alternatives:
        try:
            value = run(['dpkg-query', '-S', candidate])
        except subprocess.CalledProcessError:
            continue
        lines = value.splitlines()
        if len(lines) == 1 and lines[0].endswith(': ' + candidate):
            package = lines[0].removesuffix(': ' + candidate)
            return {'name': package, 'version': run(['dpkg-query', '-W', '-f=${Version}', package])}
    raise ValueError('unowned-library')


def preflight():
    # The binaries still honour the former variable names for one release (#505).
    if os.getuid() == 0 or any(os.environ.get(k) for k in ('TUNA_SHELL_BIN', 'TUNA_PROOF_SHELL_BIN', 'ROOST_SHELL_BIN', 'ROOST_PROOF_SHELL_BIN', 'WAYLAND_SOCKET', 'WAYLAND_DISPLAY')):  # tuna-rename: keep
        raise ValueError('installed-route-override')
    if 'ID=ubuntu' not in Path('/etc/os-release').read_text().splitlines() or 'VERSION_ID="26.04"' not in Path('/etc/os-release').read_text().splitlines():
        raise ValueError('selected-distro-required')
    version = run(['dpkg-query', '-W', '-f=${Version}', 'roost'])
    if run(['dpkg', '--verify', 'roost']):
        raise ValueError('installed-package-modified')
    files = {}
    for name in BINS:
        path = '/usr/bin/' + name
        package = owner(path)
        if package['name'] != 'roost' or package['version'] != version or run([path, '--version']) != name + ' ' + version:
            raise ValueError('installed-binary-contract')
        files[name] = digest(path)
    for package, floor in (('libgtk4-layer-shell0', '1.1'), ('libgtk-4-1', '4.12'), ('libadwaita-1-0', '1.0')):
        actual = run(['dpkg-query', '-W', '-f=${Version}', package])
        run(['dpkg', '--compare-versions', actual, 'ge', floor])
    fixture_root = Path('/usr/libexec/roost-deb-proof')
    manifest_path = fixture_root / 'provenance.json'
    digest(manifest_path)
    companion = bounded_json(manifest_path)
    if type(companion) is not dict or set(companion) != {'commit', 'files'} or type(companion['commit']) is not str or type(companion['files']) is not dict or companion['commit'] != run(['git', 'rev-parse', 'HEAD']) or set(companion['files']) != {'icon-probe', 'shortcut-inhibit-client'}:
        raise ValueError('companion-source-contract')
    for name, expected in companion['files'].items():
        if digest(fixture_root / name)['sha256'] != expected:
            raise ValueError('companion-binary-contract')
    return {'version': version, 'files': files, 'companions': companion, 'scope': 'Ubuntu26.04-installed-nested-package; service/PAM fixtures separate'}


def process(pid, expected, files):
    if type(pid) is not int or pid <= 0:
        raise ValueError('pid-type')
    root = Path('/proc') / str(pid)
    before = (root / 'stat').read_text()
    tail = before.rsplit(')', 1)[1].split()
    start, parent = int(tail[19]), int(tail[1])
    uids = [int(n) for n in next(line for line in (root / 'status').read_text().splitlines() if line.startswith('Uid:')).split()[1:]]
    exe = os.readlink(root / 'exe')
    if uids != [os.getuid()] * 4 or exe != '/usr/bin/' + expected:
        raise ValueError('original-process-required')
    # Follow the kernel's proc executable magic link deliberately. A readlink
    # and a hash of its named path cannot bind the inode this process executes.
    fd = os.open(root / 'exe', os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        executed = os.fstat(fd)
        identity = lambda s: (s.st_dev, s.st_ino, s.st_uid, s.st_mode, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
        original = files[expected]
        if (not stat.S_ISREG(executed.st_mode) or executed.st_uid != 0 or executed.st_mode & 0o022
                or (executed.st_dev, executed.st_ino, executed.st_size) !=
                   (original['dev'], original['ino'], original['size'])):
            raise ValueError('executed-file-identity')
        if digest(exe) != original:
            raise ValueError('original-process-required')
        after = (root / 'stat').read_text().rsplit(')', 1)[1].split()
        after_uids = [int(n) for n in next(line for line in (root / 'status').read_text().splitlines() if line.startswith('Uid:')).split()[1:]]
        if (int(after[19]) != start or int(after[1]) != parent or after_uids != uids
                or os.readlink(root / 'exe') != exe):
            raise ValueError('process-replaced')
        if identity(os.fstat(fd)) != identity(executed) or identity(os.stat(root / 'exe')) != identity(executed):
            raise ValueError('executed-file-replaced')
    finally:
        os.close(fd)
    return {'pid': pid, 'uid': os.getuid(), 'start': start, 'parent': parent, 'exe': exe, 'binary': files[expected]}


def libraries(pid):
    with open('/proc/' + str(pid) + '/maps', 'rb') as stream:
        data = stream.read(2 * 1024 * 1024 + 1)
    if len(data) > 2 * 1024 * 1024:
        raise ValueError('maps-limit')
    paths = {}
    for row in data.decode().splitlines():
        fields = row.split(None, 5)
        if len(fields) == 6 and fields[5].startswith('/') and '.so' in fields[5]:
            path = fields[5]
            if ' (deleted)' in path or not path.startswith(('/usr/lib/', '/lib/')) or path.startswith('/usr/local/'):
                raise ValueError('foreign-loaded-library')
            resolved = str(Path(path).resolve(strict=True))
            major, minor = (int(v, 16) for v in fields[3].split(':'))
            mapped = (os.makedev(major, minor), int(fields[4]))
            actual = os.stat(resolved, follow_symlinks=False)
            if mapped != (actual.st_dev, actual.st_ino) or not mapped[1]:
                raise ValueError('mapped-file-identity')
            if resolved in paths and paths[resolved] != mapped:
                raise ValueError('ambiguous-mapped-file')
            paths[resolved] = mapped
    if not 1 <= len(paths) <= 512:
        raise ValueError('library-count')
    receipt = []
    for path, mapped in sorted(paths.items()):
        package = owner(path)
        file = digest(path)
        if mapped != (file['dev'], file['ino']):
            raise ValueError('mapped-file-replaced')
        receipt.append({'path': path, 'package': package, 'file': file})
    for token in ('libgtk-4.so.', 'libadwaita-1.so.', 'libgtk4-layer-shell.so.'):
        if not any(token in p for p in paths):
            raise ValueError('preferred-gtk-libraries-missing')
    return receipt


def peer(path, pid):
    before = os.stat(path, follow_symlinks=False)
    if not stat.S_ISSOCK(before.st_mode) or before.st_uid != os.getuid():
        raise ValueError('session-socket-owner')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(3)
        connection.connect(str(path))
        actual_pid, uid, _gid = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
    after = os.stat(path, follow_symlinks=False)
    if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino) or uid != os.getuid() or actual_pid != pid:
        raise ValueError('session-socket-principal')
    return {'dev': before.st_dev, 'ino': before.st_ino, 'uid': uid, 'pid': actual_pid}


def observe(pid, socket_name, base, previous=None, replacement=False):
    compositor = process(pid, 'roost-compositor', base['files'])
    children = Path('/proc/' + str(pid) + '/task/' + str(pid) + '/children').read_text().split()
    if len(children) > 64:
        raise ValueError('child-limit')
    candidates = []
    for child in children:
        try:
            if os.readlink('/proc/' + child + '/exe') == '/usr/bin/roost-shell-gtk':
                candidates.append(process(int(child), 'roost-shell-gtk', base['files']))
        except FileNotFoundError:
            continue
    if len(candidates) != 1 or candidates[0]['parent'] != pid:
        raise ValueError('preferred-supervised-shell-required')
    shell = candidates[0]
    runtime = Path(os.environ['XDG_RUNTIME_DIR'])
    sockets = {name: peer(runtime / name, pid) for name in (socket_name, 'roost-' + socket_name + '.control')}
    if previous:
        if compositor != previous['compositor'] or sockets != previous['sockets']:
            raise ValueError('original-session-replaced')
        if (shell == previous['shell']) == replacement:
            raise ValueError('shell-replacement-contract')
    loaded = libraries(shell['pid'])
    if process(shell['pid'], 'roost-shell-gtk', base['files']) != shell or process(pid, 'roost-compositor', base['files']) != compositor:
        raise ValueError('observation-race')
    return {'compositor': compositor, 'shell': shell, 'sockets': sockets, 'libraries': loaded}


def bounded_json(path):
    with open(path, 'rb') as stream:
        body = stream.read(1024 * 1024 + 1)
    if len(body) > 1024 * 1024:
        raise ValueError('receipt-limit')
    return json.loads(body)


def main(args):
    if len(args) == 2 and args[0] == 'preflight':
        value = preflight()
        output = args[1]
    elif len(args) == 6 and args[0] in ('start', 'stable', 'replacement'):
        phase, pid, socket_name, base_path, previous_path, output = args
        base = bounded_json(base_path)
        previous = None if phase == 'start' else bounded_json(previous_path)
        value = observe(int(pid), socket_name, base, previous, phase == 'replacement')
    else:
        raise ValueError('fixed-arguments')
    Path(output).write_text(json.dumps(value, sort_keys=True, indent=2) + '\n')


if __name__ == '__main__':
    try:
        main(sys.argv[1:])
    except (OSError, ValueError, subprocess.SubprocessError, KeyError, TypeError, RecursionError) as failure:
        # Report only the fixed-vocabulary reason token, never raw paths or
        # command output, so the rejection stays attributable in CI logs.
        if isinstance(failure, ValueError):
            reason = str(failure)[:64]
        else:
            reason = type(failure).__name__
        print(f'installed-deb-proof: provenance rejected ({reason})', file=sys.stderr)
        sys.exit(1)
