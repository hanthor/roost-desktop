#!/usr/bin/env python3
"""Actual owned Python-child lifetime policy; no QEMU/GNOME/metric qualification."""
import ast
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('lifetime', Path(__file__).with_name('perf_vm_lifetime.py'))
life = importlib.util.module_from_spec(spec); spec.loader.exec_module(life)


class Lifetime(unittest.TestCase):
    def child(self):
        return subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])

    def test_actual_transport_failure_still_reaps_original_child(self):
        with tempfile.TemporaryDirectory() as directory:
            handoff = life.Handoff(directory, 'diagnostic', 'gnome', 1)
            child = self.child()
            original = OSError('private transport text')
            class Agent:
                def close(self): raise original
            try:
                handoff.attach(child)
                with self.assertRaises(OSError) as raised:
                    life.cleanup_owned_vm(child, Agent(), handoff)
                self.assertIs(raised.exception, original)
                self.assertIsNotNone(child.returncode)
                self.assertTrue(life.departed(handoff.value['child']))
                receipt = life.read_receipt(handoff.path, private=True)
                self.assertEqual(receipt['state'], 'reaped')
                self.assertIs(receipt['vm_departure_only'], True)
                self.assertNotIn('private', handoff.path.read_text())
            finally:
                child.kill(); child.wait(); handoff.close()

    def test_original_timeout_kills_and_reaps_only_owned_child(self):
        with tempfile.TemporaryDirectory() as directory:
            handoff = life.Handoff(directory, 'diagnostic', 'gnome', 1)
            child = subprocess.Popen([sys.executable, '-c',
                    'import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print("ready",flush=True); time.sleep(30)'],
                    stdout=subprocess.PIPE, text=True)
            try:
                self.assertEqual(child.stdout.readline(), 'ready\n')
                handoff.attach(child)
                actual_wait = child.wait
                calls = 0
                def wait(timeout=None):
                    nonlocal calls
                    calls += 1
                    if calls == 1:
                        self.assertEqual(timeout, 10)
                        raise subprocess.TimeoutExpired('controlled owned child', timeout)
                    return actual_wait(timeout=timeout)
                with patch.object(child, 'wait', side_effect=wait):
                    life.cleanup_owned_vm(child, handoff=handoff)
                self.assertEqual(child.returncode, -signal.SIGKILL)
                self.assertEqual(life.read_receipt(handoff.path, private=True)['state'], 'reaped')
                self.assertTrue(life.departed(handoff.value['child']))
            finally:
                child.kill(); child.wait(); child.stdout.close(); handoff.close()

    def test_original_main_finally_preserves_primary_metric_exception(self):
        source = ast.parse((ROOT / 'scripts/roost-vm-perf').read_text())
        main = next(n for n in source.body if isinstance(n, ast.FunctionDef) and n.name == 'main')
        original_try = next(n for n in main.body if isinstance(n, ast.Try))
        body = [ast.Raise(exc=ast.Name(id='original', ctx=ast.Load()), cause=None)]
        actual = ast.fix_missing_locations(ast.Module(body=[ast.Try(body=body, handlers=original_try.handlers,
                   orelse=[], finalbody=original_try.finalbody)], type_ignores=[]))
        original = RuntimeError('original private metric error')
        with tempfile.TemporaryDirectory() as directory:
            handoff = life.Handoff(directory, 'diagnostic', 'gnome', 1); child = self.child()
            class Agent:
                def close(self): raise OSError('private cleanup body')
            try:
                handoff.attach(child)
                scope = {'original': original, 'primary': None, 'vm': child, 'agent': Agent(),
                         'handoff': handoff, 'gnome_trace_active': False,
                         'cleanup_owned_vm': life.cleanup_owned_vm, 'sys': sys}
                with contextlib.redirect_stderr(io.StringIO()) as err, self.assertRaises(RuntimeError) as raised:
                    exec(compile(actual, 'actual-worker-finally', 'exec'), scope)
                self.assertIs(raised.exception, original)
                self.assertIsNotNone(child.returncode)
                self.assertEqual(life.read_receipt(handoff.path, private=True)['state'], 'reaped')
                self.assertNotIn('private', err.getvalue())
            finally:
                child.kill(); child.wait()

    def test_failed_owned_cleanup_or_handoff_write_cannot_publish_terminal(self):
        for case in ('wait', 'receipt'):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                handoff = life.Handoff(directory, 'diagnostic', 'gnome', 1); child = self.child()
                try:
                    handoff.attach(child)
                    if case == 'wait':
                        with patch.object(child, 'wait', side_effect=OSError('controlled wait failure')):
                            with self.assertRaises(OSError): life.cleanup_owned_vm(child, handoff=handoff)
                    else:
                        with patch.object(handoff, 'write', side_effect=OSError('controlled receipt failure')):
                            with self.assertRaises(OSError): life.cleanup_owned_vm(child, handoff=handoff)
                    self.assertEqual(life.read_receipt(handoff.path, private=True)['state'], 'running')
                finally:
                    child.kill(); child.wait(); handoff.close()

    def test_already_reaped_child_never_establishes_running_authority(self):
        with tempfile.TemporaryDirectory() as directory:
            handoff = life.Handoff(directory, 'diagnostic', 'gnome', 1)
            child = subprocess.Popen([sys.executable, '-c', 'pass'])
            child.wait(timeout=5)
            try:
                with self.assertRaises(RuntimeError): handoff.attach(child)
                self.assertEqual(life.read_receipt(handoff.path, private=True)['state'], 'launching')
                self.assertIsNone(handoff.pidfd)
            finally: handoff.close()

    def test_missing_child_admission_and_same_named_receipt_replacement_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            handoff = life.Handoff(directory, 'diagnostic', 'gnome', 1); child = self.child()
            try:
                with self.assertRaises(RuntimeError): handoff.reaped(child)
                self.assertEqual(life.read_receipt(handoff.path, private=True)['state'], 'launching')
                moved = Path(directory) / 'original'; handoff.path.rename(moved)
                handoff.path.write_text(moved.read_text()); handoff.path.chmod(0o600)
                with self.assertRaises(RuntimeError): handoff.write()
            finally:
                child.kill(); child.wait(); handoff.close()

    def status(self, root, **changes):
        value = {'schema': 1, 'profile': 'diagnostic', 'exit_status': 9, 'case_index': 1,
                 'repeat': 1, 'desktop': 'gnome', 'stage': 'measurement', 'all_six_commands_succeeded': False}
        value.update(changes)
        (root / 'acquisition-status.json').write_text(json.dumps(value))
        (root / 'acquisition-status.json').chmod(0o600)

    def test_real_worker_sigkill_nonterminal_handoff_forbids_stock_disk_operations(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); out = root / 'perf-artifacts/gnome'; out.mkdir(parents=True)
            program = """import sys,time,subprocess
sys.path.insert(0,sys.argv[1])
from perf_vm_lifetime import Handoff
h=Handoff(sys.argv[2],'diagnostic','gnome',1)
p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
h.attach(p)
time.sleep(30)
"""
            worker = subprocess.Popen([sys.executable, '-c', program, str(ROOT / 'scripts/lib'), str(out)])
            childfd = None
            try:
                deadline = time.monotonic() + 5
                while True:
                    try:
                        row = life.read_receipt(out / 'host-vm-lifecycle.json', private=True)
                        if row['state'] == 'running': break
                    except (FileNotFoundError, RuntimeError, json.JSONDecodeError): pass
                    if time.monotonic() >= deadline: self.fail('actual controlled child admission timed out')
                    time.sleep(.01)
                self.assertEqual(row['worker'], life.process(worker.pid))
                childfd = os.pidfd_open(row['child']['pid'])
                self.assertEqual(life.process(row['child']['pid']), row['child'])
                parent = int(Path(f"/proc/{row['child']['pid']}/status").read_text().split('PPid:', 1)[1].splitlines()[0])
                self.assertEqual(parent, worker.pid)
                worker.kill(); worker.wait(timeout=5)
                self.assertEqual(life.read_receipt(out / 'host-vm-lifecycle.json', private=True)['state'], 'running')
                self.status(root / 'perf-artifacts')
                with self.assertRaises(RuntimeError): life.require_stock_handoff(root / 'perf-artifacts')
                lib = root / 'scripts/lib'; lib.mkdir(parents=True)
                shutil.copyfile(ROOT / 'scripts/lib/perf_vm_lifetime.py', lib / 'perf_vm_lifetime.py')
                commands = root / 'commands'; commands.mkdir(); marker = root / 'unexpected-operations'
                for name in ('sudo', 'truncate', 'mkdir', 'rm'):
                    path = commands / name
                    path.write_text('#!/bin/sh\nprintf attempted >> "$OPERATIONS"\nexit 41\n'); path.chmod(0o755)
                env = dict(os.environ, PATH=str(commands) + ':' + os.environ['PATH'], OPERATIONS=str(marker))
                result = subprocess.run(['bash', str(ROOT / 'scripts/roost-perf-profile-pairs'), 'stock'],
                                        cwd=root, env=env, capture_output=True, text=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(marker.exists(), 'no image/disk operation before original departure admission')
            finally:
                if worker.poll() is None: worker.kill(); worker.wait(timeout=5)
                if childfd is not None:
                    signal.pidfd_send_signal(childfd, signal.SIGKILL)
                    os.close(childfd)

    def test_actual_terminal_child_and_worker_departure_is_required(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); out = root / 'gnome'; out.mkdir()
            program = """import sys,subprocess
sys.path.insert(0,sys.argv[1])
from perf_vm_lifetime import Handoff,cleanup_owned_vm
h=Handoff(sys.argv[2],'diagnostic','gnome',1)
p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
try:
 h.attach(p)
 cleanup_owned_vm(p,handoff=h)
finally: h.close()
"""
            worker = subprocess.Popen([sys.executable, '-c', program, str(ROOT / 'scripts/lib'), str(out)])
            worker.wait(timeout=5)
            self.assertEqual(worker.returncode, 0)
            original = life.require_reaped(out / 'host-vm-lifecycle.json', 'gnome', 1)
            self.status(root)
            self.assertEqual(life.require_stock_handoff(root)['diagnostic_cases_reaped'], 1)
            for changes in ({'state': 'running'}, {'pidfd_departed': False},
                            {'child_returncode': True}, {'repeat': True}, {'vm_departure_only': False},
                            {'worker': life.process(os.getpid())}, {'child': life.process(os.getpid())},
                            {'worker': dict(original['worker'], uid=True)}, {'extra': 'private'}):
                row = dict(original, **changes)
                (out / 'host-vm-lifecycle.json').write_text(json.dumps(row))
                with self.subTest(fields=list(changes)), self.assertRaises(RuntimeError):
                    life.require_stock_handoff(root)

    def test_original_status_read_replacement_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); self.status(root, stage='image-build')
            path = root / 'acquisition-status.json'
            original_stat = os.fstat
            calls = 0
            def changed(fd):
                nonlocal calls
                value = original_stat(fd); calls += 1
                if calls == 2:
                    path.rename(root / 'original-status')
                    path.write_text('{}'); path.chmod(0o600)
                return value
            with patch.object(life.os, 'fstat', side_effect=changed), self.assertRaises(RuntimeError):
                life.require_stock_handoff(root)

    def test_stock_schema_and_reached_case_policy_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.status(root, case_index=0, repeat=0, desktop='none', stage='setup')
            self.assertEqual(life.require_stock_handoff(root)['diagnostic_cases_reaped'], 0)
            self.status(root, stage='image-build')
            self.assertEqual(life.require_stock_handoff(root)['diagnostic_cases_reaped'], 0)
            for changes in ({}, {'case_index': True}, {'exit_status': True}, {'stage': 'private body'},
                            {'repeat': 2}, {'all_six_commands_succeeded': True}, {'extra': 'private'}):
                self.status(root, **changes)
                with self.subTest(changes=changes), self.assertRaises((RuntimeError, FileNotFoundError)):
                    life.require_stock_handoff(root)
            (root / 'acquisition-status.json').unlink()
            with self.assertRaises(FileNotFoundError): life.require_stock_handoff(root)

    def test_original_receipt_fd_bound_symlink_private_mode_and_size(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'receipt'; path.write_text('{}'); path.chmod(0o600)
            self.assertEqual(life.read_receipt(path, private=True), {})
            alias = path.with_name('alias'); alias.symlink_to(path)
            with self.assertRaises(OSError): life.read_receipt(alias, private=True)
            path.chmod(0o644)
            with self.assertRaises(RuntimeError): life.read_receipt(path, private=True)
            path.chmod(0o600); path.write_bytes(b'x' * 4097)
            with self.assertRaises(RuntimeError): life.read_receipt(path, private=True)


if __name__ == '__main__': unittest.main()
