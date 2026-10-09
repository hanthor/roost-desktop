#!/usr/bin/env python3
"""Exercise the actual FD-based portal artifact finalizer with bounded failures."""
from contextlib import nullcontext
import importlib.util
import os
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('finalizer', Path(__file__).with_name('tuna-portal-artifact-finalize.py'))
f = importlib.util.module_from_spec(spec)
spec.loader.exec_module(f)


class FinalizationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        for name in f.REQUIRED:
            (self.root / name).write_text('actual proof receipt\n')
        self.addCleanup(self.temp.cleanup)

    def run_finalizer(self, status=0, chown=None, stat_hook=None, **bounds):
        original_stat, original_fstat = os.stat, os.fstat
        def owned(function):
            def result(*args, **kwargs):
                values = list(function(*args, **kwargs))
                values[4:6] = [0, 0]
                result = os.stat_result(values)
                return stat_hook(result, args) if stat_hook else result
            return result
        with patch.object(f.os, 'chown', side_effect=chown), patch.object(f.os, 'fchown'), \
                patch.object(f.os, 'stat', side_effect=owned(original_stat)), \
                patch.object(f.os, 'fstat', side_effect=owned(original_fstat)), \
                (patch.multiple(f, **bounds) if bounds else nullcontext()):
            return f.finalize(str(self.root), status)

    def test_complete_receipts(self):
        result = self.run_finalizer()
        self.assertTrue(result['qualified_finalization'])
        self.assertEqual(len(result['seen']), 4)
        self.assertTrue(all(row['finalized'] and 'observed_device' in row for row in result['seen']))

    def test_observed_regular_gvfs_rotation_only(self):
        directory = self.root / 'data/gvfs-metadata'
        directory.mkdir(parents=True)
        log = directory / 'root-4e9e4e19.log'
        log.write_text('rotating log')
        def disappear(name, *args, **kwargs):
            if name == log.name:
                os.unlink(name, dir_fd=kwargs['dir_fd'])
                raise FileNotFoundError(2, 'rotation', name)
        result = self.run_finalizer(chown=disappear)
        self.assertTrue(result['qualified_finalization'])
        self.assertEqual(result['optional_disappearances'][0]['path'], 'data/gvfs-metadata/root-4e9e4e19.log')

    def test_other_missing_file_fails(self):
        def disappear(name, *args, **kwargs):
            if name == 'assertions.txt':
                os.unlink(name, dir_fd=kwargs['dir_fd'])
                raise FileNotFoundError(2, 'missing', name)
        result = self.run_finalizer(chown=disappear)
        self.assertFalse(result['qualified_finalization'])
        self.assertEqual(result['optional_disappearances'], [])

    def test_permission_failure_preserved(self):
        def denied(*args, **kwargs):
            raise PermissionError(1, 'permission denied')
        result = self.run_finalizer(23, denied)
        self.assertFalse(result['qualified_finalization'])
        self.assertEqual(result['proof_status'], 23)
        self.assertTrue((self.root / 'artifact-finalization-error.json').is_file())

    def test_empty_required_receipt_fails(self):
        (self.root / 'assertions.txt').write_text('')
        self.assertFalse(self.run_finalizer()['qualified_finalization'])

    def test_replaced_required_receipt_fails(self):
        def replace(name, *args, **kwargs):
            if name == 'session-identity.txt':
                replacement = self.root / 'replacement'
                replacement.write_text('replacement')
                os.replace(replacement, self.root / 'assertions.txt')
        self.assertFalse(self.run_finalizer(chown=replace)['qualified_finalization'])

    def test_symlink_target_never_changed(self):
        outside = self.root.parent / (self.root.name + '-outside')
        outside.write_text('outside')
        self.addCleanup(outside.unlink)
        (self.root / 'link').symlink_to(outside)
        calls = []
        def record(name, *args, **kwargs):
            calls.append((name, kwargs['follow_symlinks']))
        self.assertTrue(self.run_finalizer(chown=record)['qualified_finalization'])
        self.assertEqual(outside.read_text(), 'outside')
        self.assertIn(('link', False), calls)

    def test_entry_bound_retains_failure_receipt(self):
        result = self.run_finalizer(MAX_ENTRIES=1)
        self.assertFalse(result['qualified_finalization'])
        self.assertLessEqual(len(result['seen']), 1)
        self.assertTrue((self.root / 'artifact-finalization-error.json').is_file())

    def test_wrong_gid_fails(self):
        def wrong_gid(result, args):
            values = list(result)
            values[5] = 1000
            return os.stat_result(values)
        self.assertFalse(self.run_finalizer(stat_hook=wrong_gid)['qualified_finalization'])

    def test_device_change_fails(self):
        counts = {}
        def changed(result, args):
            if args and args[0] == 'assertions.txt':
                counts['file'] = counts.get('file', 0) + 1
                if counts['file'] > 1:
                    values = list(result)
                    values[2] += 1
                    return os.stat_result(values)
            return result
        self.assertFalse(self.run_finalizer(stat_hook=changed)['qualified_finalization'])

    def test_actual_wrapper_preserves_primary_nonzero_status(self):
        for name in ('run.sh', 'settings-run.sh'):
            wrapper = (Path(__file__).parents[2] / 'tests/portal-reference' / name).read_text()
            tail = wrapper[wrapper.index('finalization=0'):wrapper.index('fi\n')]
            for proof, cleanup, expected in ((23, 1, 23), (0, 1, 1), (0, 0, 0)):
                code = f'status={proof}\npython3() {{ return {cleanup}; }}\n' + tail
                result = subprocess.run(['bash', '-c', code], capture_output=True)
                self.assertEqual(result.returncode, expected, (name, proof, cleanup, result.stderr))

    def test_root_symlink_refused(self):
        link = self.root.parent / (self.root.name + '-link')
        link.symlink_to(self.root, target_is_directory=True)
        self.addCleanup(link.unlink)
        with self.assertRaises(OSError):
            f.finalize(str(link), 0)

    def test_error_bound_retains_failure_receipt(self):
        def denied(*args, **kwargs):
            raise PermissionError(1, 'denied')
        result = self.run_finalizer(chown=denied, MAX_ERRORS=1)
        self.assertEqual(len(result['errors']), 1)
        self.assertFalse(result['qualified_finalization'])
        self.assertTrue((self.root / 'artifact-finalization-error.json').is_file())


if __name__ == '__main__':
    unittest.main()
