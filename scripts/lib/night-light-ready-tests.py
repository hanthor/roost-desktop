#!/usr/bin/python3
"""Source-only fixed readiness contract/principal negatives; no fake-provider proof."""
import importlib.util
from pathlib import Path
import os
import subprocess
import time
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).with_name('roost-night-light-ready.py')
spec = importlib.util.spec_from_file_location('night_ready', SOURCE)
ready = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ready)


class Readiness(unittest.TestCase):
    def color(self):
        return {'Temperature': 6500, 'NightLightActive': False, 'DisabledUntilTomorrow': False,
                'Sunrise': -1.0, 'Sunset': -1.0}

    def test_real_color_contract_not_screencast_fields(self):
        self.assertEqual(ready.properties('color', self.color()),
                         {k: self.color()[k] for k in ('Temperature', 'NightLightActive', 'DisabledUntilTomorrow')})
        with self.assertRaises(RuntimeError):
            ready.properties('color', {'version': 5, 'AvailableCursorModes': 1, 'AvailableSourceTypes': 1})

    def test_color_missing_or_mistyped_fields_refused(self):
        for key in self.color():
            if key in ('Sunrise', 'Sunset'):
                continue
            value = self.color(); del value[key]
            with self.assertRaises(RuntimeError):
                ready.properties('color', value)
        for key, value in (('Temperature', True), ('Temperature', 999), ('Temperature', 10001),
                           ('Temperature', 6500.0), ('NightLightActive', 1), ('DisabledUntilTomorrow', None)):
            values = self.color(); values[key] = value
            with self.assertRaises(RuntimeError):
                ready.properties('color', values)

    def test_screenshot_version2_only_and_integer_type(self):
        for kind in ('backend', 'frontend'):
            self.assertEqual(ready.properties(kind, {'version': 2}), {'version': 2})
            for value in (True, 1, 2.0, None, '2', 0x100000000):
                with self.assertRaises(RuntimeError):
                    ready.properties(kind, {'version': value})
            with self.assertRaises(RuntimeError):
                ready.properties(kind, {'AvailableCursorModes': 1, 'AvailableSourceTypes': 1})

    def test_actual_probe_original_principal_and_service_selector(self):
        original = {'pid': 41, 'uid': 1000, 'start': 22, 'exe': '/usr/libexec/gsd-color', 'sha256': 'a' * 64}
        for kind in ready.SPECS:
            name, path, interface, exe, _package = ready.SPECS[kind]
            expected = dict(original, exe=exe)
            for case in ('valid', 'wrong-pid', 'pid-bool', 'wrong-uid', 'uid-bool', 'changed-owner', 'changed-pid', 'changed-process'):
                counts = {}
                def broker(method, value):
                    counts[method] = counts.get(method, 0) + 1
                    if method == 'GetNameOwner':
                        self.assertEqual(value, name)
                        return ':1.42' if case == 'changed-owner' and counts[method] == 2 else ':1.41'
                    self.assertEqual(value, ':1.41')
                    if method == 'GetConnectionUnixProcessID':
                        return True if case == 'pid-bool' else (42 if case == 'wrong-pid' or case == 'changed-pid' and counts[method] == 2 else 41)
                    if method == 'GetConnectionUnixUser':
                        return True if case == 'uid-bool' else (1001 if case == 'wrong-uid' else 1000)
                    raise AssertionError('unknown broker query')
                calls = []
                def props(owner, actual_path, actual_interface):
                    self.assertEqual((owner, actual_path, actual_interface), (':1.41', path, interface))
                    calls.append('actual fixed property read')
                    return self.color() if kind == 'color' else {'version': 2}
                def process(pid, start, actual_exe):
                    self.assertEqual((pid, start, actual_exe), (41, 22, exe))
                    return dict(expected, start=23) if case == 'changed-process' else expected
                if case == 'valid':
                    actual = ready.probe(kind, expected, broker, props, process)
                    self.assertEqual(actual['process'], expected)
                    self.assertEqual(len(calls), 1)
                else:
                    with self.assertRaises(RuntimeError):
                        ready.probe(kind, expected, broker, props, process)
                    if case in ('wrong-pid', 'pid-bool', 'wrong-uid', 'uid-bool', 'changed-process'):
                        self.assertEqual(calls, [])

    def test_process_scalar_types_refused_before_any_io(self):
        for pid, start in ((True, 22), (0, 22), (-1, 22), (41, True), (41, 0)):
            with self.assertRaises(RuntimeError), patch.object(ready, 'bounded') as bounded:
                ready.process(pid, start, '/usr/libexec/gsd-color')
            bounded.assert_not_called()

    def test_actual_ordinary_kernel_process_identity_and_wrong_start(self):
        self.assertEqual(os.getuid(), 1000, 'this kernel principal policy test requires actual UID1000')
        # A genuine owned root-installed Python child proves the kernel guard;
        # it is not presented as a Color/portal provider.
        child = subprocess.Popen(['/usr/bin/python3', '-c', 'import time; time.sleep(10)'])
        try:
            exe = str(Path('/usr/bin/python3').resolve(strict=True))
            deadline = time.monotonic() + 2
            while os.readlink(f'/proc/{child.pid}/exe') != exe:
                if time.monotonic() >= deadline: raise RuntimeError('original fixture exec deadline')
                time.sleep(.01)
            start = int(Path(f'/proc/{child.pid}/stat').read_text().rsplit(')', 1)[1].split()[19])
            identity = ready.process(child.pid, start, exe)
            self.assertEqual(identity['pid'], child.pid)
            self.assertEqual(identity['uid'], 1000)
            self.assertEqual(identity['exe_inode'], os.stat(f'/proc/{child.pid}/exe').st_ino)
            with self.assertRaises(RuntimeError):
                ready.process(child.pid, start + 1, exe)
        finally:
            child.terminate()
            child.wait(timeout=5)

    def test_actual_process_completion_rejects_executable_metadata_replacement(self):
        # Execute production process() with fixed PID/start/UID/path, changing
        # only final executed/named metadata. This is a policy seam, not a
        # genuine Color provider or RPM proof.
        exe = str(Path('/usr/bin/python3').resolve(strict=True))
        original = os.stat(exe)
        stat_body = b'41 (controlled) ' + (' '.join(['S'] + ['0'] * 18 + ['22'])).encode()
        status_body = b'Uid:\t1000\t1000\t1000\t1000\n'
        with tempfile.TemporaryDirectory() as directory:
            replacement_path = Path(directory) / 'replacement'
            replacement_path.write_bytes(b'controlled replacement executable')
            replacement_inode = replacement_path.stat().st_ino
            def changed(key, value):
                fields = {name: getattr(original, name) for name in
                          ('st_dev','st_ino','st_uid','st_mode','st_size','st_mtime_ns','st_ctime_ns')}
                fields[key] = value
                return SimpleNamespace(**fields)
            variants = [('valid', original, original),
                        ('executed-inode', changed('st_ino', replacement_inode), original),
                        ('named-inode', original, changed('st_ino', replacement_inode)),
                        ('executed-size', changed('st_size', original.st_size + 1), original),
                        ('named-mode', original, changed('st_mode', original.st_mode | 0o022)),
                        ('executed-mtime', changed('st_mtime_ns', original.st_mtime_ns + 1), original)]
            for case, last_mapped, last_named in variants:
                calls = []
                def bounded(path, limit, proc=False, expected=None):
                    calls.append((str(path), expected))
                    if str(path).endswith('/stat'): return stat_body
                    if str(path).endswith('/status'): return status_body
                    self.assertEqual((str(path), expected), (exe, ready.resource_key(original)))
                    return b'controlled original digest bytes'
                with self.subTest(case=case), patch.object(ready, 'bounded', side_effect=bounded), \
                     patch.object(ready.os, 'readlink', return_value=exe), \
                     patch.object(ready.os, 'stat', side_effect=[original, original, last_mapped, last_named]):
                    if case == 'valid': self.assertEqual(ready.process(41, 22, exe)['exe_inode'], original.st_ino)
                    else:
                        with self.assertRaises(RuntimeError): ready.process(41, 22, exe)
                self.assertEqual(len(calls), 5)

    def test_original_hash_fd_required_despite_transient_named_substitution(self):
        # Real distinct regular files/FDs: the named original is already
        # restored when the substituted hash FD is read. Matching final path
        # metadata alone cannot authorize this different original FD.
        with tempfile.TemporaryDirectory() as directory:
            original = Path(directory) / 'original'; original.write_bytes(b'original bytes')
            substitute = Path(directory) / 'substitute'; substitute.write_bytes(b'substituted bytes')
            expected = ready.resource_key(original.stat())
            self.assertEqual(ready.bounded(original, 64, expected=expected), b'original bytes')
            real_open = os.open
            def substitute_fd(path, flags):
                return real_open(substitute if Path(path) == original else path, flags)
            with patch.object(ready.os, 'open', side_effect=substitute_fd), self.assertRaises(RuntimeError):
                ready.bounded(original, 64, expected=expected)
            self.assertEqual(ready.resource_key(original.stat()), expected)
            # Existing default bounded callers retain their original behavior.
            self.assertEqual(ready.bounded(original, 64), b'original bytes')
    def test_package_wrong_owner_or_non51_rejected(self):
        for kind in ('color', 'backend'):
            for values in (['wrong-owner'], [ready.SPECS[kind][4], '50.1'], [ready.SPECS[kind][4], '52.0'], [ready.SPECS[kind][4], '51.0', 'modified file']):
                with patch.object(ready, 'command', side_effect=values):
                    with self.assertRaises(RuntimeError):
                        ready.package(kind)
            with patch.object(ready, 'command', side_effect=[ready.SPECS[kind][4], '51.0', '']):
                self.assertEqual(ready.package(kind)['version'], '51.0')


if __name__ == '__main__':
    unittest.main()
