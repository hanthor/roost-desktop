#!/usr/bin/python3
"""Verify PID identity, missing counters and session liveness failures."""
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import time
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader('soak', str(Path(__file__).resolve().parents[1] / 'tuna-vm-soak'))
spec = importlib.util.spec_from_loader(loader.name, loader)
soak = importlib.util.module_from_spec(spec)
loader.exec_module(soak)
from endurance_presentation import Reader


class PresentationFailures(unittest.TestCase):
    def test_actual_native_batch_rollover_preserves_cross_batch_interval(self):
        root = Path(__file__).parent / 'fixtures'
        raw = (root / 'endurance-rollover.jsonl').read_bytes()
        import hashlib
        provenance = json.loads((root / 'endurance-rollover-provenance.json').read_text())
        self.assertEqual(hashlib.sha256(raw).hexdigest(), provenance['raw_sha256'])
        result = Reader(io.BytesIO(raw), 16).finish()
        self.assertEqual(result['batches'], 2)
        self.assertEqual(result['frames'], 274)
        self.assertEqual(result['interval_ms']['count'], 273)

    def raw(self):
        return (Path(__file__).parent / 'fixtures/endurance-control.jsonl').read_bytes()

    def rows(self):
        return [json.loads(line) for line in self.raw().splitlines()]

    def encoded(self, rows):
        return b''.join(json.dumps(row).encode() + b'\n' for row in rows)

    def test_real_native_client_feedback(self):
        result = Reader(io.BytesIO(self.raw()), 3).finish()
        self.assertTrue(result['complete'])
        self.assertEqual(result['frames'], 54)
        self.assertEqual(result['interval_ms']['count'], 53)
        self.assertEqual(sum(result['flags'].values()), 0)

    def test_short_rehearsal_cannot_qualify_24_hours(self):
        with self.assertRaises(ValueError):
            Reader(io.BytesIO(self.raw()), 86400).finish()

    def test_truncated_or_missing_completion_cannot_qualify(self):
        for raw in (self.raw()[:-1], self.raw().splitlines(keepends=True)[0]):
            with self.subTest(size=len(raw)), self.assertRaises(ValueError):
                Reader(io.BytesIO(raw), 3).finish()

    def test_partial_row_can_resume_without_losing_feedback(self):
        raw = self.raw()
        stream = io.BytesIO(raw[:100])
        reader = Reader(stream, 3)
        self.assertEqual(reader.poll()['frames'], 0)
        stream.seek(100); stream.write(raw[100:]); stream.seek(0)
        self.assertEqual(reader.finish()['frames'], 54)

    def test_duplicate_backward_and_missing_frames_fail(self):
        for fault in ('duplicate', 'backwards', 'missing', 'accounting', 'batch', 'clock'):
            rows = self.rows()
            if fault == 'duplicate': rows[0]['frames'][1]['commit'] = rows[0]['frames'][0]['commit']
            elif fault == 'backwards': rows[0]['frames'][1]['presented_ns'] = 1
            elif fault == 'missing': rows[0]['frames'].pop()
            elif fault == 'accounting': rows[0]['pending'] += 1
            elif fault == 'batch': rows[0]['batch'] = 1
            elif fault == 'clock': rows[0]['clock_id'] = True
            with self.subTest(fault=fault), self.assertRaises(ValueError):
                Reader(io.BytesIO(self.encoded(rows)), 3).finish()

    def test_no_records_after_completion_or_oversized_rows(self):
        for raw in (self.raw() + self.raw(), b'x' * (128 * 1024 + 1)):
            with self.assertRaises(ValueError):
                Reader(io.BytesIO(raw), 3).finish()


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
        self.assertEqual(soak.sample(os.getuid(), self.root)['pss_kib'], 200)

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

    def test_departure_check_handles_proc_disappearing_during_read(self):
        for error in (FileNotFoundError(), ProcessLookupError()):
            with patch.object(soak, 'identity', side_effect=error):
                self.assertTrue(soak.is_departed(self.root / '100'))
        with patch.object(soak, 'identity', side_effect=PermissionError()):
            with self.assertRaises(PermissionError):
                soak.is_departed(self.root / '100')
        self.assertFalse(soak.is_departed(self.root / '100'))

    def test_departure_during_error_check_still_fails_critical_liveness(self):
        with patch.object(soak, 'identity', side_effect=[
            ProcessLookupError(), FileNotFoundError(), ('S', 1234), ('S', 1234),
        ]):
            with self.assertRaisesRegex(RuntimeError, 'critical session process'):
                soak.sample(os.getuid(), self.root)

    def test_unreadable_live_process_is_fatal(self):
        with patch.object(soak, 'identity', side_effect=PermissionError('denied')):
            with self.assertRaises(PermissionError):
                soak.sample(os.getuid(), self.root)


class HelperFailures(unittest.TestCase):
    def test_output_budget_is_enforced_even_after_exit(self):
        with self.assertRaisesRegex(RuntimeError, 'output budget'):
            soak.call([sys.executable, '-c', 'print("x" * 65537)'])

    def test_nonzero_exit_is_fatal(self):
        with self.assertRaisesRegex(RuntimeError, r'failed \(7\)'):
            soak.call([sys.executable, '-c', 'raise SystemExit(7)'])

    def test_deadline_kills_helper_process_group(self):
        with tempfile.TemporaryDirectory() as directory:
            pidfile = Path(directory) / 'pid'
            code = ('import os,time; from pathlib import Path; '
                    'pid=os.fork(); '
                    f'Path({str(pidfile)!r}).write_text(str(pid)) if pid else None; '
                    'time.sleep(30)')
            with self.assertRaisesRegex(RuntimeError, 'deadline'):
                soak.call([sys.executable, '-c', code], timeout=0.3)
            pid = int(pidfile.read_text())
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                proc = Path('/proc') / str(pid)
                if soak.is_departed(proc):
                    break
                time.sleep(0.02)
            else:
                self.fail('helper descendant survived the deadline')


if __name__ == '__main__':
    unittest.main()
