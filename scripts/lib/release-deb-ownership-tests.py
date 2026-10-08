#!/usr/bin/env python3
"""Small genuine Debian metadata fixtures; no product build or executable run."""
import gzip
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('ownership', Path(__file__).with_name('release-deb-ownership.py'))
M = importlib.util.module_from_spec(spec)
spec.loader.exec_module(M)


def fixture_tar(kind, uid=0, gid=0, root_mode=0o755):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode='w', format=tarfile.USTAR_FORMAT) as archive:
        root = tarfile.TarInfo('.')
        root.type = tarfile.DIRTYPE
        root.uid, root.gid, root.mode = uid, gid, root_mode
        root.uname = root.gname = 'root'  # Numeric fields alone decide ownership.
        archive.addfile(root)
        member = tarfile.TarInfo('./control' if kind == 'control' else './usr/bin/roost-compositor')
        member.uid, member.gid, member.mode = uid, gid, 0o644
        member.uname = member.gname = 'root'
        body = b'Package: metadata-fixture\nVersion: 1\nArchitecture: all\nDescription: metadata only\n' if kind == 'control' else b'NOT AN EXECUTABLE: METADATA FIXTURE ONLY\n'
        member.size = len(body)
        archive.addfile(member, io.BytesIO(body))
    return stream.getvalue()


def fixture_deb(path, data=None, control=None):
    result = bytearray(b'!<arch>\n')
    for name, raw in [('debian-binary', b'2.0\n'),
                      ('control.tar.gz', gzip.compress(control or fixture_tar('control'))),
                      ('data.tar.gz', gzip.compress(data or fixture_tar('data')))]:
        result.extend(f'{name+"/":<16}{0:<12}{0:<6}{0:<6}{0o100644:<8o}{len(raw):<10}`\n'.encode('ascii'))
        result.extend(raw)
        if len(raw) % 2:
            result.extend(b'\n')
    path.write_bytes(result)


class OriginalArchiveOwnership(unittest.TestCase):
    def check(self, **changes):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'metadata-only.deb'
            fixture_deb(path, **changes)
            return M.check(path)

    def test_actual_dpkg_streams_both_original_archives(self):
        self.assertEqual(self.check(), {'data': 2, 'control': 2})

    def test_each_numeric_data_and_control_owner_and_group(self):
        for kind in ('data', 'control'):
            for field in ('uid', 'gid'):
                with self.subTest(kind=kind, field=field), self.assertRaisesRegex(ValueError, 'numeric-ownership'):
                    self.check(**{kind: fixture_tar(kind, **{field: 1001})})

    def test_original_mktemp_root_mode_refused(self):
        with self.assertRaisesRegex(ValueError, 'root-mode'):
            self.check(data=fixture_tar('data', root_mode=0o700))

    def test_metadata_not_extraction_owner_decides(self):
        # Even uname/gname='root' and invoking root cannot conceal numeric1001.
        with self.assertRaisesRegex(ValueError, 'numeric-ownership'):
            M.validate_tar(fixture_tar('data', uid=1001), 'data')

    def test_empty_or_malformed_original_tar_rejected(self):
        for raw in (b'', b'not a tar'):
            with self.subTest(raw=raw), self.assertRaises((ValueError, tarfile.TarError)):
                M.validate_tar(raw, 'data')

    def test_failed_original_dpkg_command_never_qualifies(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'not-a-deb'
            path.write_bytes(b'invalid metadata-only fixture')
            with self.assertRaisesRegex(ValueError, 'command-failed'):
                M.check(path)

    def test_fixed_kind_and_output_bounds(self):
        from unittest.mock import patch
        with self.assertRaisesRegex(ValueError, 'bounds'):
            M.validate_tar(fixture_tar('data'), 'private-kind')
        with patch.object(M, 'MAX_TAR', 512), tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'metadata-only.deb'
            fixture_deb(path)
            with self.assertRaisesRegex(ValueError, 'output-bound'):
                M.check(path)


if __name__ == '__main__':
    unittest.main()
