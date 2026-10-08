#!/usr/bin/env python3
"""Strict audio evidence negatives; no fake backend playback qualification."""
import copy
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

path = Path(__file__).resolve().parents[2] / 'packaging/marlin/vm-lane/roost-vm-bell-proof'
loader = importlib.machinery.SourceFileLoader('bell_proof', str(path))
spec = importlib.util.spec_from_loader(loader.name, loader)
f = importlib.util.module_from_spec(spec)
with patch.dict(os.environ, {'XDG_RUNTIME_DIR': '/tmp/native-bell-tests'}):
    loader.exec_module(f)


class StrictPcm(unittest.TestCase):
    def actual_shape(self, audible=True):
        return {'stats': f.pcm_stats((b'\x01\x00' if audible else b'\0\0') * 48000),
                'armed': {'baseline_bytes': 24000}, 'client_before': {'rings': 3},
                'wire': {'rings': 4}, 'client_after': {'rings': 4},
                'before': {'requests': 9}, 'after': {'requests': 10},
                'player_observations': [{'pid': 123}] if audible else []}

    def test_full_capture_statistics(self):
        self.assertEqual(f.pcm_stats(b'\0' * 96000)['nonzero'], 0)
        stats = f.pcm_stats(b'\xff\x7f' * 48000)
        self.assertEqual((stats['peak'], stats['samples'], stats['nonzero']), (32767, 48000, 48000))

    def test_empty_partial_oversized_capture_rejected(self):
        for raw in (b'', b'\0' * 24000, b'\0' * 95999, b'\0' * 96002):
            with self.subTest(bytes=len(raw)), self.assertRaises(RuntimeError):
                f.pcm_stats(raw)

    def test_positive_requires_both_pcm_and_owned_player(self):
        for key, value in (('player_observations', []), ('stats', f.pcm_stats(b'\0' * 96000))):
            actual = self.actual_shape(); actual[key] = value
            with self.assertRaises(RuntimeError): f.validate_case_result('audible', actual)

    def test_negative_requires_audio_and_player_absence(self):
        for key, value in (('player_observations', [{'pid': 123}]), ('stats', f.pcm_stats(b'\1\0' * 48000))):
            actual = self.actual_shape(False); actual[key] = value
            with self.assertRaises(RuntimeError): f.validate_case_result('events-disabled', actual)

    def test_one_original_request_in_both_spaces(self):
        for section, key in (('wire', 'rings'), ('client_after', 'rings'), ('after', 'requests')):
            actual = self.actual_shape(); actual[section][key] += 1
            with self.subTest(section=section), self.assertRaises(RuntimeError):
                f.validate_case_result('audible', actual)

    def test_missing_complete_capture_or_baseline_rejected(self):
        for section, key in (('stats', 'bytes'), ('armed', 'baseline_bytes')):
            actual = self.actual_shape(); actual[section][key] -= 2
            with self.assertRaises(RuntimeError): f.validate_case_result('audible', actual)

    def test_independent_visual_pref_does_not_disable_expected_audio(self):
        f.validate_case_result('visual-independent', self.actual_shape())
        with self.assertRaises(RuntimeError): f.validate_case_result('visual-independent', self.actual_shape(False))

    def test_failed_theme_and_restored_theme_expectations(self):
        for case in ('theme-bound', 'theme-disabled'):
            f.validate_case_result(case, self.actual_shape(False))
            with self.assertRaises(RuntimeError): f.validate_case_result(case, self.actual_shape())
        f.validate_case_result('restored', self.actual_shape())
        with self.assertRaises(RuntimeError): f.validate_case_result('restored', self.actual_shape(False))

    def test_actual_original_process_identity_rejects_changed_start(self):
        actual = f.identity(os.getpid())
        original = copy.deepcopy(actual); original['start_ticks'] += 1
        with self.assertRaises(RuntimeError): f.identity(os.getpid(), original)

    def test_forged_application_pid_cannot_replace_secure_client(self):
        node = {'props': {'client.id': 7, 'application.process.id': os.getpid()}}
        obj = {'type': 'PipeWire:Interface:Client', 'id': 7,
               'info': {'props': {'pipewire.sec.pid': os.getpid() + 1, 'pipewire.sec.uid': os.getuid()}}}
        with self.assertRaises(RuntimeError): f.secure_client([obj], node, os.getpid())
        obj['info']['props']['pipewire.sec.pid'] = os.getpid()
        self.assertEqual(f.secure_client([obj], node, os.getpid())['secure_pid'], os.getpid())

    def test_actual_route_requires_original_port_nodes_and_monitor(self):
        objects = [
            {'type': 'PipeWire:Interface:Port', 'id': 11, 'info': {'direction': 'output', 'props': {'node.id': 1, 'port.monitor': True}}},
            {'type': 'PipeWire:Interface:Port', 'id': 21, 'info': {'direction': 'input', 'props': {'node.id': 2}}},
            {'type': 'PipeWire:Interface:Link', 'id': 31, 'info': {'output-node-id': 1, 'output-port-id': 11,
             'input-node-id': 2, 'input-port-id': 21, 'state': 'active'}}]
        self.assertEqual(len(f.graph_route(objects, 1, 2, monitor=True)), 1)
        for target, key, value in ((0, 'port.monitor', False), (1, 'node.id', 99)):
            invalid = copy.deepcopy(objects); invalid[target]['info']['props'][key] = value
            with self.assertRaises(RuntimeError): f.graph_route(invalid, 1, 2, monitor=True)
        self.assertEqual(f.graph_route(objects, 1, 99), [])

    def test_replaced_or_oversized_capture_cannot_be_read(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'capture'; path.write_bytes(b'\0' * 32)
            raw, original = f.bounded_file(path, 32, owner=os.getuid())
            self.assertEqual(len(raw), 32)
            other = Path(directory) / 'replacement'; other.write_bytes(b'\0' * 32); other.replace(path)
            with self.assertRaises(RuntimeError): f.bounded_file(path, 32, original)
            path.write_bytes(b'\0' * 33)
            with self.assertRaises(RuntimeError): f.bounded_file(path, 32)
            with self.assertRaises(RuntimeError): f.bounded_file(path, 64, owner=os.getuid() + 1)

    def test_graph_count_and_property_bounds(self):
        with self.assertRaises(RuntimeError): f.stream_graph([{}] * 2049)
        obj = {'id': 1, 'info': {'props': {'media.class': 'Stream/Input/Audio', 'node.name': 'x' * 257}}}
        with self.assertRaises(RuntimeError): f.stream_graph([obj])

    def test_partial_start_cleanup_restores_actual_settings_and_owned_unit(self):
        values = ['false', 'false', 'true', "'freedesktop'"]
        record = {'saved_settings': values, 'unit': 'roost-vm-bell-' + '0' * 32}
        calls = []
        def run(*args):
            calls.append(args)
            return values[f.SETTINGS.index((args[2], args[3]))] if args[:2] == ('gsettings', 'get') else ''
        with patch.object(f, 'call', side_effect=run): actual = f.restore_record(record)
        self.assertTrue(actual['restored'])
        self.assertEqual(len(actual['settings_readbacks']), 4)
        self.assertIn(('systemctl', '--user', 'stop', record['unit']), calls)

    def test_cleanup_failure_attempts_all_settings_readbacks_and_retains_failure(self):
        values = ['false', 'false', 'true', "'freedesktop'"]
        record = {'saved_settings': values, 'unit': 'roost-vm-bell-' + '0' * 32}
        calls = []
        def run(*args):
            calls.append(args)
            if args[:2] == ('gsettings', 'get'): return 'wrong'
            raise RuntimeError('confirmed cleanup failure')
        with patch.object(f, 'call', side_effect=run): actual = f.restore_record(record)
        self.assertFalse(actual['restored'])
        self.assertEqual(len(actual['settings_readbacks']), 4)
        self.assertEqual(len(calls), 9)
        self.assertEqual(len(actual['errors']), 9)

    def test_compile_gate_rejects_before_any_generation(self):
        build = path.with_name('roost-vm-bell-build')
        env = dict(os.environ, ROOST_CI_ONLY='0')
        result = subprocess.run(['bash', str(build)], env=env, capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn(b'CI-only', result.stderr)


if __name__ == '__main__':
    unittest.main()
