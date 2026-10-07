#!/usr/bin/env python3
"""Data/retention regressions; no kernel build, code generation or tracing."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch, MagicMock

REPO = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('roost_kernel_trace', str(REPO / 'packaging/marlin/perf/roost-kernel-trace'))
spec = importlib.util.spec_from_loader(loader.name, loader)
observer = importlib.util.module_from_spec(spec)
loader.exec_module(observer)
loader = importlib.machinery.SourceFileLoader('observer_perf_host', str(REPO / 'scripts/roost-vm-perf'))
spec = importlib.util.spec_from_loader(loader.name, loader)
host = importlib.util.module_from_spec(spec)
loader.exec_module(host)


class Fixture:
    def __init__(self, base=None):
        self.names = {}
        self.strings = bytearray(b'\0')
        self.records = []
        self.base = base
    def name(self, value):
        if value not in self.names:
            self.names[value] = (len(self.base.strings) if self.base else 0) + len(self.strings)
            self.strings.extend(value.encode() + b'\0')
        return self.names[value]
    def add(self, kind, name, size, count=0, payload=b'', flag=False):
        self.records.append(struct.pack('<III', self.name(name), kind << 24 | count | (0x80000000 if flag else 0), size) + payload)
        return (max(self.base.types) if self.base else 0) + len(self.records)
    def integer(self, name, size):
        return self.add(1, name, size, payload=struct.pack('<I', size * 8))
    def pointer(self, target):
        return self.add(2, '', target)
    def aggregate(self, kind, name, size, fields):
        payload = b''.join(struct.pack('<III', self.name(member), kind_id, offset * 8) for member, kind_id, offset in fields)
        return self.add(kind, name, size, len(fields), payload)
    def function(self, name, arguments, returned=0):
        payload = b''.join(struct.pack('<II', self.name('arg' + str(index)), kind_id) for index, kind_id in enumerate(arguments))
        proto = self.add(13, '', returned, len(arguments), payload)
        return self.add(12, name, proto)
    def raw(self):
        types = b''.join(self.records)
        return struct.pack('<HBBIIIII', 0xeb9f, 1, 0, 24, 0, len(types), len(types), len(self.strings)) + types + self.strings


def fixture():
    f = Fixture()
    u32 = f.integer('unsigned int', 4)
    u64 = f.integer('unsigned long long', 8)
    p64 = f.pointer(u64)
    mode = f.aggregate(4, 'drm_mode_object', 4, [('id', u32, 0)])
    minor = f.aggregate(4, 'drm_minor', 4, [('index', u32, 0)])
    minor_pointer = f.pointer(minor)
    device = f.aggregate(4, 'drm_device', 24, [('primary', minor_pointer, 16)])
    device_pointer = f.pointer(device)
    crtc = f.aggregate(4, 'drm_crtc', 40, [('dev', device_pointer, 0), ('base', mode, 24), ('index', u32, 32)])
    crtc_pointer = f.pointer(crtc)
    commit = f.aggregate(4, 'drm_atomic_commit', 8, [])
    commit_pointer = f.pointer(commit)
    pending_base = f.aggregate(4, 'drm_pending_event', 8, [('file_priv', p64, 0)])
    vbl = f.aggregate(4, 'drm_event_vblank', 8, [('user_data', u64, 0)])
    event = f.aggregate(5, '', 8, [('vbl', vbl, 0)])
    pending = f.aggregate(4, 'drm_pending_vblank_event', 40, [('base', pending_base, 0), ('event', event, 8), ('pipe', u32, 24), ('sequence', u64, 32)])
    pending_pointer = f.pointer(pending)
    node = f.aggregate(4, 'timerqueue_node', 8, [('expires', u64, 0)])
    timer = f.aggregate(4, 'hrtimer', 32, [('node', node, 16)])
    timer_pointer = f.pointer(timer)
    f.aggregate(4, 'drm_vblank_crtc_timer', 56, [('timer', timer, 8), ('interval', u64, 40), ('crtc', crtc_pointer, 48)])
    f.function('virtio_gpu_crtc_atomic_flush', [crtc_pointer, commit_pointer])
    f.function('drm_crtc_arm_vblank_event', [crtc_pointer, pending_pointer])
    f.function('drm_crtc_accurate_vblank_count', [crtc_pointer], u64)
    f.function('drm_crtc_vblank_get_vblank_timeout', [crtc_pointer, p64])
    f.function('send_vblank_event', [device_pointer, pending_pointer, u64, u64])
    f.function('drm_vblank_timer_function', [timer_pointer], u32)
    return f


class Parser(unittest.TestCase):
    def test_offsets_and_nested_union_come_from_fixture_btf(self):
        btf = observer.Btf(fixture().raw())
        definitions, offsets = observer.build_probes(btf, btf, btf, 'roost_vblank_1234567890abcdef')
        self.assertEqual(offsets['drm_crtc.base.id'], {'offset': 24, 'bytes': 4})
        self.assertEqual(offsets['drm_pending_vblank_event.event.vbl.user_data']['offset'], 8)
        self.assertTrue(any('target=+32($arg2)' in row for row in definitions))
        self.assertTrue(any('interval=+32($arg1)' in row for row in definitions), 'timer container offset must be subtracted')
        self.assertTrue(any('minor=+0(+16(+0($arg1)))' in row for row in definitions))
    def test_split_module_uses_live_base_type_and_string_ids(self):
        base = observer.Btf(fixture().raw())
        f = Fixture(base)
        f.aggregate(4, 'module_test', 8, [('value', base.named('unsigned long long', 1), 0)])
        split = observer.Btf(f.raw(), base)
        self.assertEqual(split.offset('module_test', 'value'), (0, 8))
        self.assertEqual(split.offset('drm_crtc', 'base', 'id'), (24, 4))
    def test_short_invalid_endian_and_section_bounds_rejected(self):
        raw = fixture().raw()
        for value in (b'', raw[:23], b'\x00\x00' + raw[2:], raw[:-1]):
            with self.assertRaises(ValueError): observer.Btf(value)
    def test_missing_and_duplicate_struct_names_rejected(self):
        f = fixture();f.aggregate(4, 'drm_crtc', 4, [])
        btf = observer.Btf(f.raw())
        with self.assertRaises(ValueError): btf.offset('drm_crtc', 'index')
        with self.assertRaises(ValueError): btf.offset('missing', 'index')
    def test_wrong_argument_prototype_is_not_a_proxy(self):
        btf = observer.Btf(fixture().raw())
        with self.assertRaises(ValueError): btf.signature('send_vblank_event', ['drm_crtc'], 'void')
        with self.assertRaises(ValueError): btf.signature('drm_crtc_accurate_vblank_count', ['drm_device'], 'scalar64')
    def test_bitfield_is_not_guessed_as_a_byte_offset(self):
        f = Fixture();u32=f.integer('unsigned int', 4)
        f.add(4, 'bitfield', 4, 1, struct.pack('<III', f.name('field'), u32, 3 << 24 | 1), flag=True)
        btf=observer.Btf(f.raw())
        with self.assertRaises(ValueError): btf.offset('bitfield', 'field')


class ActualRecordQualification(unittest.TestCase):
    def test_required_fields_not_faulted_or_missing(self):
        definition=['p:roost_test/roost_test_arm fn crtc=$arg1:x64 object=+1($arg1):u32 pipe=+2($arg1):u32']
        good=b'<task>-1 [000] 1.000: roost_test_arm: (fn) crtc=0xffff object=39 pipe=0\n'
        self.assertEqual(observer.validate_trace_fields(good,'roost_test',definition), {'roost_test_arm':1})
        for raw in (good.replace(b'0xffff',b'0x0'),good.replace(b'object=39',b'object=(fault)'),good.replace(b'pipe=0',b'pipe=99'),b''):
            with self.assertRaises(ValueError): observer.validate_trace_fields(raw,'roost_test',definition)
    def test_profile_names_are_unique_event_names_not_group_paths(self):
        text=b'roost_test_arm 12 0\nother_arm 40 2\n'
        with patch.object(observer,'bounded',return_value=text):
            self.assertEqual(observer.profiles('roost_test'), {'roost_test_arm':{'hits':12,'misses':0}})


class Retention(unittest.TestCase):
    def test_whole_kernel_trace_retained_before_actual_loss_rejection(self):
        import base64, hashlib
        raw=b'actual trace bytes\n';formats=b'{"actual": "format"}'
        metadata={'raw':{'bytes':len(raw),'sha256':hashlib.sha256(raw).hexdigest()},
                  'formats':{'bytes':len(formats),'sha256':hashlib.sha256(formats).hexdigest()},
                  'qualified_acquisition':False,'errors':['actual dropped events']}
        def probe(agent, action, index):
            value=raw if action=='kernel-trace-read' else formats
            return {'kernel_trace':{'index':index,'data':base64.b64encode(value).decode()}}
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',side_effect=probe):
            out=Path(tmp)
            with self.assertRaises(ValueError):host.retain_kernel_trace(None,out,metadata)
            self.assertEqual((out/'kernel-vblank-trace.txt').read_bytes(),raw)
            self.assertEqual((out/'kernel-vblank-formats.json').read_bytes(),formats)
            self.assertEqual(json.loads((out/'kernel-vblank-source.json').read_text()),metadata)
    def test_incomplete_kernel_chunk_cannot_qualify(self):
        metadata={'raw':{'bytes':2,'sha256':'bad'}}
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',return_value={'kernel_trace':{'index':0,'data':'eA=='}}):
            with self.assertRaises(ValueError):host.retain_kernel_trace(None,Path(tmp),metadata)

class CallbackIdentity(unittest.TestCase):
    def fixture(self):
        messages = [
            ('Clutter::FrameClock::dispatch()', ''),
            ('Clutter::FrameClock::presented()', ''),
            ('Roost::FrameClock::dispatch-id', 'output=Virtual-1 frame=839 dispatch_us=146603288'),
            ('Roost::FrameClock::presented-id', 'output=Virtual-1 view_frame=839 global_frame=759 presentation_us=146602970 sequence=10084 flags=1 kms_ready_us=146605881'),
            ('Roost::KMS::raw-page-flip', 'crtc=39 sequence=10084 seconds=146 microseconds=602970 device=/dev/dri/card1'),
            ('Roost::KMS::raw-page-flip-identity', 'crtc=39 sequence=10084 user_data=0x123 page_flip_data=0x456'),
            ('Roost::KMS::atomic-commit-begin', 'impl_device=0x123 update=0x789 req=0xabc fd=9 flags=513'),
            ('Roost::KMS::atomic-commit-end', 'impl_device=0x123 update=0x789 req=0xabc fd=9 flags=513 ret=0'),
            ('Roost::KMS::page-flip-listener-identity', 'crtc=39 impl_device=0x123 update=0x789 page_flip_data=0x456 listener_user_data=0xdef'),
        ]
        return {'marks':[{'pid':1135,'name':name,'message':message,'monotonic_ns':index} for index,(name,message) in enumerate(messages)]}
    def test_actual_identity_keeps_original_early_timestamp(self):
        result = host.frame_source_evidence(self.fixture(), 1135)
        self.assertEqual(result['presentations'][0]['source_time_us'],146602970)
        self.assertEqual(result['kernel_callback_identities'][0]['page_flip_data'],'0x456')
        self.assertEqual(len(result['kernel_commit_listener_observations']),3)
        self.assertNotIn('inferred_frame',result)
    def test_wrong_sequence_or_malformed_commit_cannot_qualify(self):
        decoded = self.fixture()
        decoded['marks'][5]['message'] = decoded['marks'][5]['message'].replace('sequence=10084','sequence=10085')
        with self.assertRaisesRegex(ValueError,'do not match'):
            host.frame_source_evidence(decoded,1135)
        decoded = self.fixture();decoded['marks'][6]['message'] += ' extra=guessed'
        with self.assertRaisesRegex(ValueError,'malformed actual commit'):
            host.frame_source_evidence(decoded,1135)


class OriginalRejection(unittest.TestCase):
    def test_diagnostic_failure_retains_original_bounds_and_capture(self):
        import base64, gzip
        fixture = REPO / 'scripts/lib/fixtures'
        metadata = json.loads((fixture/'gnome51-presentation-before-dispatch.json').read_text())
        raw = gzip.decompress((fixture/'gnome51-presentation-before-dispatch.syscap.gz').read_bytes())
        metadata['mutter_cogl_library'] = {'package':'mutter 51.0-1.3'}
        metadata['kernel_trace'] = {'qualified_acquisition':False,'errors':['actual loss']}
        order = []
        def chunk(agent, action, index):
            self.assertEqual(action,'gnome-trace-read')
            return {'gnome_trace':{'index':index,'data':base64.b64encode(raw[index*65536:(index+1)*65536]).decode()}}
        def kernel(*args):
            order.append('kernel')
            raise ValueError('actual loss')
        def identity(*args):
            order.append('identity')
            raise ValueError('missing diagnostic marks')
        with tempfile.TemporaryDirectory() as tmp, patch.object(host,'guest_probe',side_effect=chunk), patch.object(host,'retain_kernel_trace',side_effect=kernel), patch.object(host,'frame_source_evidence',side_effect=identity):
            out = Path(tmp)
            with self.assertRaisesRegex(host.FrameOwnershipError,'presentation precedes'):
                host.retain_gnome_trace(None,out,metadata)
            self.assertEqual(order,['kernel','identity'])
            self.assertEqual((out/'gnome-overview.syscap').read_bytes(),raw)
            rejected = json.loads((out/'gnome-overview-frame-rejection.json').read_text())
            self.assertEqual(rejected['evidence']['presentation_upper_ns'],123677789000)
            self.assertIsNone(rejected['independent_source_evidence'])
            self.assertTrue((out/'kernel-vblank-acquisition-rejection.json').exists())


class Lifecycle(unittest.TestCase):
    def test_preflight_failure_retains_unqualified_metadata(self):
        root = MagicMock()
        root.exists.return_value = True
        root.__truediv__.return_value.exists.return_value = False
        root.stat.return_value.st_uid = 0
        root.stat.return_value.st_mode = 0o40700
        root.is_symlink.return_value = False
        with patch.object(observer, 'ROOT', root), patch.object(observer, 'start_observer', side_effect=ValueError('missing actual BTF')), patch.object(observer, 'save') as save:
            with self.assertRaisesRegex(ValueError, 'missing actual BTF'):
                observer.start()
            result = save.call_args.args[1]
            self.assertFalse(result['qualified_acquisition'])
            self.assertEqual(result['stage'], 'preflight')
            self.assertEqual(result['errors'], ['missing actual BTF'])
    def test_disable_failure_does_not_skip_owned_probe_cleanup(self):
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp)
            instance = trace / 'instances/roost_test'; instance.mkdir(parents=True)
            (trace / 'kprobe_events').write_text('')
            marker = {'group':'roost_test', 'instance_created':True, 'definitions':['p:roost_test/roost_test_arm fn']}
            with patch.object(observer, 'TRACE', trace), patch.object(observer, 'write', side_effect=OSError('disable failed')):
                errors = observer.cleanup(marker)
            self.assertEqual(len(errors), 2)
            self.assertEqual((trace / 'kprobe_events').read_text(), '-:roost_test/roost_test_arm\n')
            self.assertFalse(instance.exists())
    def test_unobserved_branch_is_coverage_not_fabricated_records(self):
        definitions = ['p:roost_test/roost_test_arm fn crtc=$arg1:x64', 'p:roost_test/roost_test_timeout fn output=$arg2:x64']
        counts = observer.validate_trace_fields(b'<task>-1 [000] 1.000: roost_test_arm: (fn) crtc=0xffff\n', 'roost_test', definitions)
        self.assertEqual(counts, {'roost_test_arm':1, 'roost_test_timeout':0})


if __name__=='__main__': unittest.main()
