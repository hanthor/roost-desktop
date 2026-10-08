#!/usr/bin/env python3
"""Numeric ownership of original Debian archives, never extracted-file metadata."""
import io
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tarfile
import time

MAX_TAR = 256 * 1024 * 1024


def validate_tar(raw, kind):
    if kind not in ('data', 'control') or not 0 < len(raw) <= MAX_TAR:
        raise ValueError('archive-bounds')
    count = 0
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        for member in archive:
            count += 1
            if count > 4096:
                raise ValueError('archive-entry-bound')
            if member.uid != 0 or member.gid != 0:
                raise ValueError('archive-numeric-ownership')
            if kind == 'data' and member.name.rstrip('/') in ('.', ''):
                if not member.isdir() or member.mode != 0o755:
                    raise ValueError('archive-root-mode')
    if count == 0:
        raise ValueError('empty-archive')
    return count


def original_tar(package, kind):
    command = ['dpkg-deb', '--fsys-tarfile' if kind == 'data' else '--ctrl-tarfile', str(package)]
    child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL)
    try:
        assert child.stdout is not None
        os.set_blocking(child.stdout.fileno(), False)
        raw = bytearray()
        deadline = time.monotonic() + 15
        with selectors.DefaultSelector() as poll:
            poll.register(child.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ValueError('archive-read-deadline')
                if not poll.select(min(remaining, .1)):
                    continue
                block = os.read(child.stdout.fileno(), min(65536, MAX_TAR + 1 - len(raw)))
                if not block:
                    break
                raw.extend(block)
                if len(raw) > MAX_TAR:
                    raise ValueError('archive-output-bound')
        if child.wait(timeout=max(.01, deadline - time.monotonic())) != 0:
            raise ValueError('archive-command-failed')
        return bytes(raw)
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=5)
        if child.stdout is not None:
            child.stdout.close()


def check(package):
    return {kind: validate_tar(original_tar(package, kind), kind) for kind in ('data', 'control')}


if __name__ == '__main__':
    try:
        if len(sys.argv) != 2:
            raise ValueError('fixed-arguments')
        check(Path(sys.argv[1]))
    except (OSError, ValueError, tarfile.TarError, subprocess.SubprocessError):
        print('release-deb-ownership: original archive metadata rejected', file=sys.stderr)
        raise SystemExit(1) from None
