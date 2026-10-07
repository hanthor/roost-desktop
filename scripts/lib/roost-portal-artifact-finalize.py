#!/usr/bin/env python3
"""Bounded ownership finalization for the ordinary-user portal CI fixtures."""
import argparse
import errno
import json
import os
from pathlib import PurePosixPath
import re
import stat
import sys

MAX_ENTRIES = 8192
MAX_DEPTH = 32
MAX_PATH_BYTES = 1024
MAX_ERRORS = 128
MAX_REPORT_BYTES = 16 * 1024 * 1024
REQUIRED = ('assertions.txt', 'runtime-versions.txt', 'session-identity.txt', 'compositor-state.json')


class BoundError(RuntimeError):
    pass


def finalize(root, proof_status):
    report = {'proof_status': proof_status, 'qualified_finalization': False,
              'bounds': {'entries': MAX_ENTRIES, 'depth': MAX_DEPTH,
                         'path_bytes': MAX_PATH_BYTES, 'report_bytes': MAX_REPORT_BYTES},
              'seen': [], 'optional_disappearances': [], 'errors': []}
    saturated = False
    root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)

    root_identity = os.fstat(root_fd)

    def error(path, exception):
        nonlocal saturated
        if len(report['errors']) >= MAX_ERRORS:
            saturated = True
            return
        report['errors'].append({'path': path, 'type': type(exception).__name__, 'message': str(exception)})

    def disappearance(path, regular, exception, inode):
        candidate = PurePosixPath(path)
        allowed = (regular and exception.errno == errno.ENOENT
                   and str(candidate.parent) == 'data/gvfs-metadata'
                   and re.fullmatch(r'[A-Za-z0-9_-]+-[0-9a-f]{8}\.log', candidate.name))
        if allowed:
            report['optional_disappearances'].append({'path': path, 'observed_inode': inode,
                'reason': 'observed regular GVFS rotation log disappeared during ownership finalization'})
        return allowed

    def verify_owned(metadata):
        if metadata.st_uid not in (0, 1000):
            raise RuntimeError('artifact is not owned by the fixture root or ordinary principal')

    def walk(directory_fd, prefix, depth):
        if depth > MAX_DEPTH:
            raise BoundError('artifact directory depth exceeds bound')
        verify_owned(os.fstat(directory_fd))
        with os.scandir(directory_fd) as entries:
            for entry in entries:
                path = prefix + entry.name
                if len(report['seen']) >= MAX_ENTRIES or len(path.encode()) > MAX_PATH_BYTES:
                    raise BoundError('artifact entry/path count exceeds bound')
                inode = entry.inode()
                regular = False
                row = {'path': path, 'observed_inode': inode}
                report['seen'].append(row)
                try:
                    regular = entry.is_file(follow_symlinks=False)
                    metadata = os.stat(entry.name, dir_fd=directory_fd, follow_symlinks=False)
                    verify_owned(metadata)
                    if metadata.st_ino != inode:
                        raise RuntimeError('artifact inode changed before finalization')
                    row['observed_device'] = metadata.st_dev
                    row['mode'] = stat.S_IFMT(metadata.st_mode)
                    row['original_uid'] = metadata.st_uid
                    if stat.S_ISDIR(metadata.st_mode):
                        child = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC,
                                        dir_fd=directory_fd)
                        try:
                            if (os.fstat(child).st_ino, os.fstat(child).st_dev) != (inode, metadata.st_dev):
                                raise RuntimeError('artifact directory changed before traversal')
                            walk(child, path + '/', depth + 1)
                            os.fchown(child, 0, 0)
                        finally:
                            os.close(child)
                    else:
                        os.chown(entry.name, 0, 0, dir_fd=directory_fd, follow_symlinks=False)
                    after = os.stat(entry.name, dir_fd=directory_fd, follow_symlinks=False)
                    if (after.st_ino, after.st_dev) != (inode, metadata.st_dev) or stat.S_IFMT(after.st_mode) != row['mode'] or (after.st_uid, after.st_gid) != (0, 0):
                        raise RuntimeError('artifact identity/type/ownership changed during finalization')
                    row['finalized'] = True
                except BoundError:
                    raise
                except OSError as exception:
                    if not disappearance(path, regular, exception, inode):
                        error(path, exception)
                except Exception as exception:
                    error(path, exception)

    try:
        try:
            walk(root_fd, '', 0)
            if proof_status == 0:
                for name in REQUIRED:
                    try:
                        metadata = os.stat(name, dir_fd=root_fd, follow_symlinks=False)
                        observed = next((row for row in report['seen'] if row['path'] == name), None)
                        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_size == 0
                                or (metadata.st_uid, metadata.st_gid) != (0, 0)
                                or not observed or not observed.get('finalized')
                                or (metadata.st_ino, metadata.st_dev, stat.S_IFMT(metadata.st_mode)) !=
                                   (observed['observed_inode'], observed['observed_device'], observed['mode'])):
                            raise RuntimeError('required proof artifact is empty, replaced, or not an owned regular file')
                    except Exception as exception:
                        error(name, exception)
            os.fchown(root_fd, 0, 0)
            root_after = os.fstat(root_fd)
            if ((root_after.st_dev, root_after.st_ino) != (root_identity.st_dev, root_identity.st_ino)
                    or (root_after.st_uid, root_after.st_gid) != (0, 0)):
                raise RuntimeError('artifact root identity/ownership changed during finalization')
        except Exception as exception:
            error('', exception)
        if saturated:
            report['errors'][-1] = {'path': '', 'type': 'BoundError', 'message': 'artifact finalization error count exceeds bound'}
        report['qualified_finalization'] = not report['errors']
        raw = (json.dumps(report, ensure_ascii=False, indent=2) + '\n').encode()
        if len(raw) > MAX_REPORT_BYTES:
            report = {'proof_status': proof_status, 'qualified_finalization': False,
                      'errors': [{'message': 'artifact finalization metadata exceeds bound'}]}
            raw = (json.dumps(report) + '\n').encode()
        name = 'artifact-finalization.json' if report['qualified_finalization'] else 'artifact-finalization-error.json'
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o644, dir_fd=root_fd)
        try:
            with os.fdopen(fd, 'wb', closefd=False) as stream:
                stream.write(raw)
            os.fchown(fd, 0, 0)
        finally:
            os.close(fd)
    finally:
        os.close(root_fd)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', choices=['/out'])
    parser.add_argument('--proof-status', type=int, required=True)
    args = parser.parse_args()
    if os.geteuid() != 0 or not 0 <= args.proof_status <= 255:
        raise RuntimeError('CI fixture root and actual bounded proof exit status required')
    report = finalize(args.root, args.proof_status)
    if report['errors']:
        print(json.dumps(report['errors']), file=sys.stderr)
        return args.proof_status or 1
    return args.proof_status


if __name__ == '__main__':
    sys.exit(main())
