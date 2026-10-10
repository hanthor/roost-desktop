#!/usr/bin/env python3
"""Native overview acquisition rejects missing cards, actions and identities."""
import base64
import hashlib
import copy
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch, Mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tuna_overview_receipts as receipts
ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('overview_host', str(ROOT / 'scripts/tuna-vm-perf'))
spec = importlib.util.spec_from_loader(loader.name, loader)
host = importlib.util.module_from_spec(spec)
loader.exec_module(host)

loader = importlib.machinery.SourceFileLoader('overview_phase', str(ROOT / 'packaging/marlin/perf/tuna-perf-phase'))
spec = importlib.util.spec_from_loader(loader.name, loader)
phase = importlib.util.module_from_spec(spec)
loader.exec_module(phase)


class NativeReceipts(unittest.TestCase):
    def setUp(self):
        self.identity = {'pid': 123, 'uid': 1000, 'start_ticks': 42,
                         'path': '/usr/bin/tuna-compositor', 'sha256': 'a'*64,
                         'boot_id': '12345678-1234-1234-1234-123456789abc', 'trace_enabled': True}
        self.image = {'profile': 'diagnostic', 'schema': 1}
        self.capture = {'process': self.identity, 'reference_image': self.image,
                        'after_id': 6, 'cursor': 'original-cursor', 'boundary_journal_sha256': 'd'*64, 'events': []}
        for index in range(20):
            ident = index + 7
            start = (index+1)*1_000_000_000
            ready = {'expected': 2, 'rendered': 2, 'cached': 4, 'pending': 0}
            first = dict(ready, rendered=0, pending=2) if index == 0 else ready
            self.capture['events'] += [
                {'kind': 'input', 'schema': 2, 'id': ident, 'opening': index % 2 == 0, 'input_ns': start},
                {'kind': 'card-candidate', 'schema': 2, 'id': ident, 'candidate_ns': start+1,
                 'preparation': 'incomplete-on-first-candidate' if index == 0 else 'ready-on-first-candidate', 'cards': first},
                {'kind': 'queued', 'id': ident, 'queued_ns': start+200_000_000},
                {'kind': 'presented', 'schema': 2, 'id': ident, 'input_ns': start,
                 'queued_ns': start+200_000_000, 'presented_ns': start+216_000_000, 'sequence': 10+index,
                 'cards_complete': True, 'first_cards': first, 'presented_cards': ready}]

    def source(self, capture):
        raw = b'\n'.join(json.dumps({'_TRANSPORT': 'journal', 'SYSLOG_IDENTIFIER': 'tuna-perf-native', '_PID': str(self.identity['pid']), '_UID': str(self.identity['uid']),
               '_EXE': self.identity['path'], '_BOOT_ID': self.identity['boot_id'].replace('-', ''),
               '__CURSOR': 'cursor-'+str(index), 'MESSAGE': 'tuna-perf-input: '+json.dumps(event)}).encode()
               for index, event in enumerate(capture['events']))
        capture['journal_source'] = {'data': base64.b64encode(raw).decode(), 'bytes': len(raw),
                                     'sha256': hashlib.sha256(raw).hexdigest()}

    def validate(self):
        self.source(self.capture)
        return receipts.validate_capture(self.capture, self.identity, self.image, 'a'*64, 'b'*40, 'sha256:'+'c'*64)

    def test_cold_completion_retains_original_input_and_kernel_sequence(self):
        result = self.validate()
        self.assertTrue(result['qualifies_completed_overview'])
        self.assertEqual(len(result['samples']), 20)
        self.assertEqual(result['samples'][0]['input_to_card_complete_scanout_s'], .216)
        self.assertEqual(result['samples'][0]['first_cards']['rendered'], 0)
        self.assertEqual(result['samples'][0]['sequence'], 10)
        self.assertEqual(result['pre_input_preparation'], 'unobserved')

    def test_missing_wrong_layout_unknown_or_desktop_open_never_qualifies(self):
        self.source(self.capture)
        for expected, rendered in ((3, 2), (None, 2), (0, 0)):
            with self.subTest(expected=expected):
                capture = copy.deepcopy(self.capture)
                capture['events'][3]['presented_cards'].update(expected=expected, rendered=rendered)
                self.source(capture)
                with self.assertRaises(ValueError):
                    receipts.validate_capture(capture, self.identity, self.image, 'a'*64, 'b'*40, 'sha256:'+'c'*64)

    def test_original_raw_journal_and_parsed_events_cannot_diverge(self):
        self.source(self.capture)
        self.capture['events'][3]['presented_ns'] += 1
        with self.assertRaisesRegex(ValueError, 'original journal'):
            self.validate_raw()

    def validate_raw(self):
        return receipts.validate_capture(self.capture, self.identity, self.image, 'a'*64, 'b'*40, 'sha256:'+'c'*64)

    def test_legacy_cache_field_and_nonobject_capture_refused(self):
        capture = copy.deepcopy(self.capture)
        for row in capture['events']:
            for key in ('cards', 'first_cards', 'presented_cards'):
                if key in row:
                    row[key]['cache'] = row[key].pop('cached', 0)
        self.source(capture)
        with self.assertRaisesRegex(ValueError, 'cached'):
            receipts.validate_capture(capture, self.identity, self.image, 'a'*64, 'b'*40, 'sha256:'+'c'*64)
        with self.assertRaises(ValueError):
            receipts.validate_capture([], self.identity, self.image, 'a'*64, 'b'*40, 'sha256:'+'c'*64)

    def test_identity_source_and_image_refused(self):
        self.source(self.capture)
        for binary, source, image in (('d'*64, 'b'*40, 'sha256:'+'c'*64), ('a'*64, None, 'sha256:'+'c'*64),
                                      ('a'*64, 'b'*40, None)):
            with self.assertRaises(ValueError):
                receipts.validate_capture(self.capture, self.identity, self.image, binary, source, image)
        original = dict(self.identity, start_ticks=43)
        with self.assertRaises(ValueError):
            receipts.validate_capture(self.capture, original, self.image, 'a'*64, 'b'*40, 'sha256:'+'c'*64)

    def test_missing_duplicate_discarded_and_reused_sequence_refused(self):
        self.source(self.capture)
        for change in ('missing', 'duplicate', 'discarded', 'sequence', 'time', 'schema', 'direction'):
            capture = copy.deepcopy(self.capture)
            if change == 'missing': capture['events'].pop()
            if change == 'duplicate': capture['events'].append(capture['events'][0])
            if change == 'discarded': capture['events'][2]['kind'] = 'discarded'
            if change == 'sequence': capture['events'][7]['sequence'] = 10
            if change == 'time': capture['events'][3]['input_ns'] += 1
            if change == 'schema': capture['events'][3]['schema'] = 1
            if change == 'direction': capture['events'][0]['opening'] = False
            self.source(capture)
            with self.subTest(change=change), self.assertRaises(ValueError):
                receipts.validate_capture(capture, self.identity, self.image, 'a'*64, 'b'*40, 'sha256:'+'c'*64)

    def test_journal_trusted_fields_bind_actual_process_not_message_claim(self):
        row = {'_TRANSPORT': 'journal', 'SYSLOG_IDENTIFIER': 'tuna-perf-native', '_PID': '123', '_UID': '1000', '_EXE': self.identity['path'],
               '_BOOT_ID': self.identity['boot_id'].replace('-', ''), '__CURSOR': 'cursor',
               'MESSAGE': 'tuna-perf-input: '+json.dumps(self.capture['events'][0])}
        parsed, cursor = receipts.journal_rows(json.dumps(row).encode(), self.identity)
        self.assertEqual(parsed, [self.capture['events'][0]])
        self.assertEqual(cursor, 'cursor')
        for field in ('_TRANSPORT', 'SYSLOG_IDENTIFIER', '_PID', '_UID', '_EXE', '_BOOT_ID', '__CURSOR'):
            changed = dict(row, **{field: 'incorrect' if field != '__CURSOR' else None})
            with self.subTest(field=field), self.assertRaises(ValueError):
                receipts.journal_rows(json.dumps(changed).encode(), self.identity)
        with self.assertRaises(ValueError): receipts.journal_rows(b' '* (receipts.MAX_BYTES+1), self.identity)

    def test_inherited_stream_cannot_authenticate_compositor_receipt(self):
        row = {'_TRANSPORT': 'stdout', 'SYSLOG_IDENTIFIER': 'tuna-perf-native',
               '_PID': '123', '_UID': '1000', '_EXE': self.identity['path'],
               '_BOOT_ID': self.identity['boot_id'].replace('-', ''), '__CURSOR': 'cursor',
               'MESSAGE': 'tuna-perf-input: '+json.dumps(self.capture['events'][0])}
        with self.assertRaisesRegex(ValueError, 'transport'):
            receipts.journal_rows(json.dumps(row).encode(), self.identity)

    def test_host_retains_native_bytes_and_matches_original_package(self):
        self.source(self.capture)
        with tempfile.TemporaryDirectory() as scratch:
            out = Path(scratch)
            payload = out/'payload.json'; selection = out/'selection.json'
            payload.write_text(json.dumps({'required_members': {'usr/bin/tuna-compositor': {'sha256': 'a'*64}}}))
            selection.write_text(json.dumps({'head_sha': 'b'*40}))
            with patch.object(host, 'guest_probe', return_value={'tuna_overview': self.capture}):
                result = host.retain_tuna_overview(None, out, self.capture, self.image, payload, selection,
                                                  ['fixture_image_id=sha256:'+'c'*64])
            self.assertTrue(result['qualifies_completed_overview'])
            self.assertEqual(result['opening_input_to_card_complete_scanout_s']['count'], 10)
            self.assertTrue((out/'tuna-overview-native.json').is_file())
            payload.write_text(json.dumps({'required_members': {'usr/bin/tuna-compositor': {'sha256': 'd'*64}}}))
            with patch.object(host, 'guest_probe', return_value={'tuna_overview': self.capture}):
                result = host.retain_tuna_overview(None, out, self.capture, self.image, payload, selection,
                                                  ['fixture_image_id=sha256:'+'c'*64])
            self.assertFalse(result['qualifies_completed_overview'])
            self.assertEqual(result['status'], 'refused')

    def test_fixed_qga_actions_need_no_chunk_index(self):
        for action in ('tuna-overview-start', 'tuna-overview-stop'):
            agent = Mock()
            value = {'boottime_s': 100, 'tuna_overview': self.capture}
            agent.command.side_effect = [{'pid': 7}, {'exited': True, 'exitcode': 0,
                'out-data': base64.b64encode(json.dumps(value).encode()).decode()}]
            self.assertEqual(host.guest_probe(agent, action), value)
            self.assertEqual(agent.command.call_args_list[0].kwargs['arg'], [action])

    def test_failed_or_missing_acquisition_never_uses_first_response(self):
        with tempfile.TemporaryDirectory() as scratch, patch.object(host, 'guest_probe', side_effect=RuntimeError('missing')):
            result = host.retain_tuna_overview(None, Path(scratch), self.capture, self.image, None, None, [])
        self.assertFalse(result['qualifies_completed_overview'])
        self.assertEqual(result['status'], 'refused')


class GuestBoundary(unittest.TestCase):
    setUp = NativeReceipts.setUp
    def test_original_boundary_saved_and_used_without_retiming_input(self):
        initial = {'process': self.identity, 'reference_image': self.image, 'cursor': 'original', 'after_id': 6, 'boundary_journal_sha256': 'a'*64}
        with patch.object(phase, 'tuna_process', return_value=self.identity), \
             patch.object(phase, 'reference_image', return_value=self.image), \
             patch.object(phase, 'tuna_journal', return_value=([{'id': 6, 'kind': 'presented'}], 'original', {'sha256': 'a'*64})), \
             patch.object(phase, 'write_marker') as write:
            self.assertEqual(phase.tuna_overview('start'), initial)
            self.assertEqual(write.call_args.args[1], initial)
        with patch.object(phase, 'tuna_process', return_value=self.identity), \
             patch.object(phase, 'reference_image', return_value=self.image), \
             patch.object(phase, 'read_marker', return_value=initial), \
             patch.object(phase, 'tuna_journal', return_value=(self.capture['events'], 'last', {'sha256': 'b'*64})) as journal:
            result = phase.tuna_overview('stop')
            self.assertEqual(result['events'][0]['input_ns'], self.capture['events'][0]['input_ns'])
            journal.assert_called_once_with(self.identity, 'original')

    def test_missing_boundary_or_restarted_process_refused(self):
        with patch.object(phase, 'tuna_process', return_value=self.identity), \
             patch.object(phase, 'reference_image', return_value=self.image), \
             patch.object(phase, 'tuna_journal', return_value=([], None, {'sha256': 'a'*64})), \
             patch.object(phase, 'write_marker') as write:
            with self.assertRaises(ValueError): phase.tuna_overview('start')
            write.assert_not_called()
        initial = {'process': dict(self.identity, start_ticks=43), 'reference_image': self.image, 'cursor': 'original'}
        with patch.object(phase, 'tuna_process', return_value=self.identity), \
             patch.object(phase, 'reference_image', return_value=self.image), \
             patch.object(phase, 'read_marker', return_value=initial), \
             patch.object(phase, 'tuna_journal') as journal:
            with self.assertRaises(ValueError): phase.tuna_overview('stop')
            journal.assert_not_called()


if __name__ == '__main__': unittest.main()
