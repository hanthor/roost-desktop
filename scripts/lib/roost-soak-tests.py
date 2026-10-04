#!/usr/bin/python3
"""Verify PID identity, missing counters and session liveness failures."""
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader('soak', str(Path(__file__).resolve().parents[1] / 'roost-vm-soak'))
spec = importlib.util.spec_from_loader(loader.name, loader)
soak = importlib.util.module_from_spec(spec)
loader.exec_module(soak)


class ProcessFailures(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for index, name in enumerate(soak.CRITICAL, 100):
            proc = self.root / str(index)
            proc.mkdir()
            # Parenthesized comm can contain spaces and ')' characters.
            (proc / 'stat').write_text(f'{index} (name with ) spaces) S ' + '0 ' * 18 + '1234 ' + '0 ' * 8)
            (proc / 'exe').symlink_to('/usr/bin/' + name)
            (proc / 'smaps_rollup').write_text('Pss: 100 kB\nRss: 200 kB\n')
            (proc / 'fd').mkdir()
            (proc / 'fd/0').touch()

    def test_parenthesized_name_does_not_shift_identity(self):
        self.assertEqual(soak.identity(self.root / '100'), ('S', 1234))
        self.assertEqual(soak.sample(os.getuid(), self.root)['pss_kib'], 300)

    def test_missing_counter_fails_instead_of_zero(self):
        (self.root / '100/smaps_rollup').write_text('Rss: 200 kB\n')
        with self.assertRaisesRegex(RuntimeError, 'incomplete memory'):
            soak.sample(os.getuid(), self.root)

    def test_departed_critical_process_fails_liveness(self):
        (self.root / '100/exe').unlink()
        with self.assertRaisesRegex(RuntimeError, 'critical session process'):
            soak.sample(os.getuid(), self.root)

    def test_zombie_cannot_count_as_live_critical_process(self):
        stat = self.root / '100/stat'
        stat.write_text(stat.read_text().replace('spaces) S', 'spaces) Z'))
        with self.assertRaisesRegex(RuntimeError, 'critical session process'):
            soak.sample(os.getuid(), self.root)

    def test_pid_reuse_fails_instead_of_combining_processes(self):
        with patch.object(soak, 'identity', side_effect=[('S', 1234), ('S', 5678)]):
            with self.assertRaisesRegex(RuntimeError, 'changed identity'):
                soak.sample(os.getuid(), self.root)

    def test_unreadable_live_process_is_fatal(self):
        with patch.object(soak, 'identity', side_effect=PermissionError('denied')):
            with self.assertRaises(PermissionError):
                soak.sample(os.getuid(), self.root)


if __name__ == '__main__':
    unittest.main()
