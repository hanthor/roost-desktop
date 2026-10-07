#!/usr/bin/env python3
"""Resource accounting, process identity, trace parsing and bounded statistics."""
import importlib.machinery
import importlib.util
import json
import os
import base64
import hashlib
import io
import subprocess
import sys
from contextlib import contextmanager, redirect_stdout
from types import SimpleNamespace
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch, mock_open

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


guest = load("guest", ROOT / "scripts/lib/roost-perf-guest.py")
host = load("host", ROOT / "scripts/roost-vm-perf")
sysprof = load("sysprof", ROOT / "scripts/lib/gnome_sysprof.py")
phase = load("phase", ROOT / "packaging/marlin/perf/roost-perf-phase")
profiler = load("profiler", ROOT / "packaging/marlin/perf/roost-gnome-profiler")
diag = load("diagnostics", ROOT / "scripts/lib/perf_trace_diagnostics.py")


@contextmanager
def isolated_kernel_marker(directory):
    """Redirect the fixed /run kernel marker into a scratch directory."""
    real_path = phase.Path
    def redirect(path, *args, **kwargs):
        if str(path) == "/run/roost-perf-kernel.json":
            return real_path(directory) / "roost-perf-kernel.json"
        return real_path(path, *args, **kwargs)
    with patch.object(phase, "Path", side_effect=redirect):
        yield


@contextmanager
def isolated_trace_start(directory, run_error=None, run_result=None):
    """Drive gnome_trace start with kernel provenance and marker isolated."""
    run_mock = (patch.object(phase.subprocess, "run", side_effect=run_error)
                if run_error is not None else
                patch.object(phase.subprocess, "run", return_value=run_result))
    with patch.object(phase.pwd, "getpwnam", return_value=SimpleNamespace(pw_uid=1000)), \
         run_mock, \
         patch.object(phase, "guest_kernel_provenance", return_value={}), \
         isolated_kernel_marker(directory):
        yield


class TraceFailureDiagnostics(unittest.TestCase):
    def receipt(self):
        return {'trace_failure':{'schema':1,'action':'start','stage':'start-call','error_class':'RuntimeError'}}
    def test_profiler_run_redacts_exception_body_and_preserves_failure(self):
        def failed(context):
            context.stage='start-call'
            raise RuntimeError('PRIVATE arbitrary user session text')
        output=io.StringIO()
        with patch.object(profiler,'main',failed),patch.object(sys,'argv',['profiler','start']),redirect_stdout(output):
            self.assertEqual(profiler.run(),1)
        self.assertEqual(diag.decode(output.getvalue(),'start'),self.receipt())
        self.assertNotIn('PRIVATE',output.getvalue())
    def test_unknown_glib_domain_and_remote_body_are_replaced_by_fixed_other(self):
        class Error(Exception):
            domain='PRIVATE-domain'
            code=37
        context=diag.Context('start');context.stage='start-call'
        value=context.failure(Error('PRIVATE body'),Error,lambda _: 'org.private.UserPayload.secret')
        raw=diag.encode(value,'start')
        self.assertNotIn(b'PRIVATE',raw);self.assertNotIn(b'secret',raw)
        self.assertEqual(value['trace_failure']['glib'],{'domain':'other','code':37,'remote_name':'other'})
    def test_exact_pinned_drained_messages_classify_without_transporting_text(self):
        class Error(Exception):
            domain='g-dbus-error-quark'
            code=0
        context=diag.Context('start');context.stage='start-call'
        remote=lambda _: 'org.freedesktop.DBus.Error.Failed'
        source=(ROOT/'packaging/marlin/perf/mutter-profiler/drained-capture-start.patch').read_text()
        for message,label in diag.DRAINED_FAILURES.items():
            self.assertIn('"'+message+'"',source)
            for prefix in ('','GDBus.Error:org.freedesktop.DBus.Error.Failed: '):
                error=Error('PRIVATE body');error.message=prefix+message
                receipt=context.failure(error,Error,remote)
                raw=diag.encode(receipt,'start')
                self.assertEqual(diag.decode(raw,'start')['trace_failure']['drained_failure'],label)
                self.assertNotIn(message.encode(),raw);self.assertNotIn(b'PRIVATE',raw)
    def test_unknown_partial_private_or_oversized_message_never_classifies(self):
        class Error(Exception):
            domain='g-dbus-error-quark'
            code=0
        context=diag.Context('start');context.stage='start-call'
        remote=lambda _: 'org.freedesktop.DBus.Error.Failed'
        known=next(iter(diag.DRAINED_FAILURES))
        for message in (None,{},known+' PRIVATE',known[:-1],'PRIVATE '+known,'x'*513,
                        'GDBus.Error:org.private.Error: '+known):
            error=Error('PRIVATE body');error.message=message
            receipt=context.failure(error,Error,remote)
            self.assertNotIn('drained_failure',receipt['trace_failure'])
            self.assertNotIn(b'PRIVATE',diag.encode(receipt,'start'))
    def test_classification_requires_original_start_failure_controls(self):
        valid={'trace_failure':{'schema':1,'action':'start','stage':'start-call','error_class':'GLibError',
               'glib':{'domain':'g-dbus-error-quark','code':0,'remote_name':'org.freedesktop.DBus.Error.Failed'},
               'drained_failure':'owned-view'}}
        self.assertEqual(diag.decode(diag.encode(valid,'start'),'start'),valid)
        for field,value in [('drained_failure','PRIVATE'),('drained_failure',[]),('stage','stop-call'),('error_class','RuntimeError')]:
            row=json.loads(json.dumps(valid));row['trace_failure'][field]=value
            with self.assertRaises(ValueError):diag.encode(row,'start')
        for field,value in [('domain','g-io-error-quark'),('remote_name','org.freedesktop.DBus.Error.NoReply')]:
            row=json.loads(json.dumps(valid));row['trace_failure']['glib'][field]=value
            with self.assertRaises(ValueError):diag.encode(row,'start')
        row=json.loads(json.dumps(valid));row['trace_failure']['action']='stop'
        with self.assertRaises(ValueError):diag.encode(row,'stop')
    def test_phase_transports_valid_failure_and_observed_nonzero_only(self):
        raw=diag.encode(self.receipt(),'start').decode()
        error=subprocess.CalledProcessError(7,['fixed'],output=raw,stderr='PRIVATE STDERR')
        output=io.StringIO()
        with tempfile.TemporaryDirectory() as marker_dir,isolated_trace_start(marker_dir,run_error=error),redirect_stdout(output):
            with self.assertRaises(SystemExit) as failed:phase.gnome_trace('start',0)
        self.assertEqual(failed.exception.code,7)
        self.assertEqual(diag.decode(output.getvalue(),'start'),self.receipt())
        self.assertNotIn('PRIVATE',output.getvalue())
    def test_host_retains_receipt_before_original_guest_failure(self):
        raw=diag.encode(self.receipt(),'start')
        class Agent:
            def command(self,name,**kwargs):
                return {'pid':1} if name=='guest-exec' else {'exited':True,'exitcode':7,'out-data':base64.b64encode(raw).decode(),'err-data':base64.b64encode(b'PRIVATE STDERR').decode()}
        with tempfile.TemporaryDirectory() as directory:
            out=Path(directory)
            with self.assertRaisesRegex(RuntimeError,'guest performance gnome-trace-start failed'):
                host.guest_probe(Agent(),'gnome-trace-start',out=out)
            retained=(out/'gnome-trace-start-failure.json').read_text()
            self.assertNotIn('PRIVATE',retained)
            self.assertEqual(json.loads(retained)['diagnostic'],self.receipt())
            self.assertEqual(json.loads(retained)['guest_exitcode'],7)
    def test_malformed_unknown_oversized_duplicate_and_boolean_fields_rejected(self):
        cases=[b'PRIVATE-not-json',b'x'*4097,b'['*1500+b']'*1500, b'{"trace_failure":{},"trace_failure":{}}']
        for field,value in [('message','PRIVATE'),('stage','PRIVATE'),('schema',True),
                            ('principal',{'uid':True,'pid':3,'owner':':1.4'}),('stage',[]),('error_class',{}),('component',[]),
                            ('glib',{'domain':'g-io-error-quark','code':True,'remote_name':'other'}),
                            ('glib',{'domain':[],'code':1,'remote_name':'other'})]:
            row=self.receipt();row['trace_failure'][field]=value;cases.append(json.dumps(row).encode())
        for raw in cases:
            with self.subTest(raw=raw[:40]),self.assertRaises((ValueError,TypeError)):diag.decode(raw,'start')
            output=io.StringIO()
            error=subprocess.CalledProcessError(9,['fixed'],output=raw.decode(),stderr='PRIVATE STDERR')
            with tempfile.TemporaryDirectory() as marker_dir,isolated_trace_start(marker_dir,run_error=error),redirect_stdout(output):
                with self.assertRaises(SystemExit) as phase_failure:phase.gnome_trace('start',0)
            self.assertEqual(phase_failure.exception.code,9)
            rejected=diag.decode(output.getvalue(),'start')['trace_failure']
            self.assertTrue(rejected['payload_rejected'])
            self.assertEqual(rejected['subprocess_returncode'],9)
            self.assertEqual(rejected['stage'],'diagnostic-transport')
            self.assertNotIn('PRIVATE',output.getvalue())
            with tempfile.TemporaryDirectory() as directory:
                out=Path(directory)
                host.retain_trace_failure({'exitcode':9,'out-data':base64.b64encode(raw).decode()},'gnome-trace-start',out)
                retained=(out/'gnome-trace-start-failure.json').read_text()
                self.assertNotIn('PRIVATE',retained)
                self.assertTrue(json.loads(retained)['diagnostic_rejected'])
    def test_mapped_library_failure_reports_fixed_component_and_actual_bound_check(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'library';path.write_bytes(b'known')
            original=path.stat()
            context=diag.Context('start');context.public['component']='cogl'
            with self.assertRaises(RuntimeError):
                profiler.mapped_library_digest(path,original.st_dev,original.st_ino+1,context=context)
            self.assertEqual(context.stage,'mutter-library-validation')
            value=context.failure(RuntimeError('PRIVATE path/error'))
            self.assertEqual(value['trace_failure']['component'],'cogl')
            self.assertNotIn('PRIVATE',diag.encode(value,'start').decode())
    def test_zero_exit_cannot_turn_structured_failure_into_acquisition_success(self):
        raw=diag.encode(self.receipt(),'start').decode()
        with tempfile.TemporaryDirectory() as marker_dir,isolated_trace_start(marker_dir,run_result=SimpleNamespace(stdout=raw,returncode=0)):
            with self.assertRaisesRegex(ValueError,'cannot qualify'):phase.gnome_trace('start',0)
        row=dict(self.receipt(),boottime_s=100)
        class Agent:
            def command(self,name,**kwargs):
                return {'pid':1} if name=='guest-exec' else {'exited':True,'exitcode':0,'out-data':base64.b64encode(json.dumps(row).encode()).decode()}
        with tempfile.TemporaryDirectory() as directory:
            out=Path(directory)
            with self.assertRaisesRegex(RuntimeError,'failed'):host.guest_probe(Agent(),'gnome-trace-start',out=out)
            retained=json.loads((out/'gnome-trace-start-failure.json').read_text())
            self.assertEqual(retained['guest_exitcode'],0)
            self.assertTrue(retained['diagnostic_rejected'])
    def test_malformed_inner_exit9_reaches_host_as_safe_rejection_not_success(self):
        output=io.StringIO()
        failure=subprocess.CalledProcessError(9,['fixed'],output='PRIVATE raw body',stderr='PRIVATE stderr')
        with tempfile.TemporaryDirectory() as marker_dir,isolated_trace_start(marker_dir,run_error=failure),redirect_stdout(output):
            with self.assertRaises(SystemExit) as exit_status:phase.gnome_trace('start',0)
        raw=output.getvalue().encode()
        class Agent:
            def command(self,name,**kwargs):
                return {'pid':1} if name=='guest-exec' else {'exited':True,'exitcode':exit_status.exception.code,'out-data':base64.b64encode(raw).decode()}
        with tempfile.TemporaryDirectory() as directory:
            out=Path(directory)
            with self.assertRaisesRegex(RuntimeError,'failed'):host.guest_probe(Agent(),'gnome-trace-start',out=out)
            value=json.loads((out/'gnome-trace-start-failure.json').read_text())
            self.assertEqual(value['guest_exitcode'],9)
            self.assertTrue(value['diagnostic']['trace_failure']['payload_rejected'])
            self.assertNotIn('PRIVATE',json.dumps(value))
    def test_rejected_payload_transport_fields_are_exact_and_cannot_claim_success(self):
        for code in (True,0,256,-256):
            row=self.receipt();row['trace_failure'].update(stage='diagnostic-transport',error_class='CalledProcessError',payload_rejected=True,subprocess_returncode=code)
            with self.assertRaises(ValueError):diag.decode(json.dumps(row),'start')
        row=self.receipt();row['trace_failure']['payload_rejected']=False
        with self.assertRaises(ValueError):diag.decode(json.dumps(row),'start')
    def test_trace_read_and_wrong_action_cannot_accept_failure_as_success(self):
        with self.assertRaises(ValueError):diag.decode(diag.encode(self.receipt(),'start'),'stop')
        with self.assertRaises(ValueError):diag.decode(diag.encode(self.receipt(),'start'),'read')
    def test_cancellation_is_not_converted_to_a_diagnostic_failure(self):
        with patch.object(profiler,'main',side_effect=KeyboardInterrupt),patch.object(sys,'argv',['profiler','start']),redirect_stdout(io.StringIO()) as output:
            with self.assertRaises(KeyboardInterrupt):profiler.run()
            self.assertEqual(output.getvalue(),'')


class MutterLibraryIdentity(unittest.TestCase):
    def test_multiple_segments_share_one_kernel_library_identity(self):
        maps = "1000-2000 r--p 0 08:01 42 /usr/lib/mutter-51/libmutter-cogl-51.so.0.0.0\n"
        maps += "2000-3000 r-xp 1000 08:01 42 /usr/lib/mutter-51/libmutter-cogl-51.so.0.0.0\n"
        self.assertEqual(profiler.mapped_cogl_library(maps),
                         ("/usr/lib/mutter-51/libmutter-cogl-51.so.0.0.0", os.makedev(8, 1), 42))
        for bad in (maps.replace("51.so", "50.so"), maps + maps.replace(" 42 ", " 43 "),
                    maps.replace(".so.0.0.0", ".so.0.0.0 (deleted)")):
            with self.subTest(maps=bad), self.assertRaises(RuntimeError):
                profiler.mapped_cogl_library(bad)

    def test_all_diagnostic_modules_require_unique_live_mapping_identities(self):
        maps = "".join(f"1000-2000 r-xp 0 08:01 {42 + i} /usr/lib/{name}.so.0.0.0\n"
                       for i, name in enumerate(profiler.MUTTER_MODULES.values()))
        for i, (component, name) in enumerate(profiler.MUTTER_MODULES.items()):
            with self.subTest(component=component):
                expected = (f"/usr/lib/{name}.so.0.0.0", os.makedev(8, 1), 42 + i)
                self.assertEqual(profiler.mapped_mutter_library(maps, component), expected)
                segment = next(row for row in maps.splitlines() if f"/{name}.so" in row)
                for bad in (maps.replace(segment, ""),
                            maps.replace(segment, segment + " (deleted)"),
                            maps + segment.replace(f" {42 + i} ", " 99 ") + "\n"):
                    with self.assertRaises(RuntimeError):
                        profiler.mapped_mutter_library(bad, component)
        with self.assertRaises(ValueError):
            profiler.mapped_mutter_library(maps, "unknown")

    def test_mapped_module_collection_rejects_foreign_ownership_and_remapping(self):
        import io
        maps = "".join(f"1000-2000 r-xp 0 08:01 {42 + i} /usr/lib/{name}.so.0.0.0\n"
                       for i, name in enumerate(profiler.MUTTER_MODULES.values()))

        def fingerprint(path, device, inode, context=None):
            return dict(path=path, device=device, inode=inode, uid=0, bytes=100,
                        sha256="a" * 64)

        def package(arguments, **unused):
            return "mutter 51.0-1.2\n" if arguments[1:3] == ["-Q", "mutter"] else "mutter\n"

        with patch.object(profiler.Path, "open", mock_open(read_data=maps)), \
                patch.object(profiler, "mapped_library_digest", side_effect=fingerprint), \
                patch.object(profiler.subprocess, "check_output", side_effect=package):
            self.assertEqual(set(profiler.mutter_libraries_provenance(123)), {"core", "clutter", "cogl"})
            with patch.object(profiler.subprocess, "check_output",
                              side_effect=lambda arguments, **unused:
                                  package(arguments) if arguments[1:3] == ["-Q", "mutter"] else "foreign\n"):
                with self.assertRaisesRegex(RuntimeError, "different package"):
                    profiler.mutter_libraries_provenance(123)
            with patch.object(profiler, "mapped_library_digest",
                              side_effect=lambda *args, **kwargs: dict(fingerprint(*args, **kwargs), uid=1000)):
                with self.assertRaisesRegex(RuntimeError, "not owned by root"):
                    profiler.mutter_libraries_provenance(123)
            with patch.object(profiler.Path, "open", side_effect=[io.StringIO(maps),
                              io.StringIO(maps.replace(" 44 ", " 99 "))]):
                with self.assertRaisesRegex(RuntimeError, "mappings changed"):
                    profiler.mutter_libraries_provenance(123)

    def test_replaced_mapped_path_is_rejected_even_with_identical_bytes(self):
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "library.so"
            path.write_bytes(b"mapped library evidence")
            path.chmod(0o644)
            identity = path.stat()
            fingerprint = profiler.mapped_library_digest(str(path), identity.st_dev, identity.st_ino)
            self.assertEqual(fingerprint["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
            replacement = path.with_name("replacement.so")
            replacement.write_bytes(path.read_bytes())
            replacement.chmod(0o644)
            replacement.replace(path)
            with self.assertRaisesRegex(RuntimeError, "identity changed"):
                profiler.mapped_library_digest(str(path), identity.st_dev, identity.st_ino)


class KernelBuildEvidence(unittest.TestCase):
    @staticmethod
    def note(name=b"GNU\0", descriptor=b"12345678901234567890", kind=3):
        import struct
        return (struct.pack("<III", len(name), len(descriptor), kind) + name +
                b"\0" * ((-len(name)) % 4) + descriptor + b"\0" * ((-len(descriptor)) % 4))

    def test_reads_live_build_id_among_other_notes(self):
        raw = self.note(b"Linux\0", b"other", 0) + self.note()
        result = phase.gnu_build_id(raw)
        self.assertEqual(result["build_id"], b"12345678901234567890".hex())
        self.assertEqual(bytes.fromhex(result["gnu_note_hex"]), self.note())

    def test_missing_duplicate_truncated_and_oversized_notes_are_denied(self):
        for bad in (b"", self.note(b"Linux\0"), self.note() * 2,
                    self.note()[:-1], self.note() + b"x", self.note(descriptor=b""),
                    self.note(descriptor=b"x" * 65), b"\xff" * 12):
            with self.subTest(raw=bad), self.assertRaises(ValueError):
                phase.gnu_build_id(bad)

    def test_kernel_file_final_symlink_and_nonregular_files_are_denied(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "target").write_bytes(b"kernel payload")
            (root / "link").symlink_to(root / "target")
            with self.assertRaises(OSError):
                phase.kernel_file_digest(root / "link")
            with self.assertRaises(RuntimeError):
                phase.kernel_file_digest(root)

    def test_hyphenated_module_filename_is_accepted_inside_running_kernel_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tree = root / "kernel/drivers/gpu/drm/virtio"
            tree.mkdir(parents=True)
            for name in ("virtio-gpu.ko.zst", "virtio-gpu.ko", "virtio_gpu.ko.xz"):
                path = tree / name
                path.write_bytes(b"module path fixture")
                self.assertEqual(phase.virtio_module_path(str(path), root), path)
            outside = root / "virtio-gpu.ko.zst"
            outside.write_bytes(b"wrong tree")
            foreign = tree / "different-gpu.ko.zst"
            foreign.write_bytes(b"wrong module")
            (tree / "escape").symlink_to(outside)
            for path in (outside, foreign, tree / "escape"):
                with self.subTest(path=path), self.assertRaises(RuntimeError):
                    phase.virtio_module_path(str(path), root)

    def test_missing_and_changed_kernel_evidence_preserves_complete_capture(self):
        import hashlib
        fixture = ROOT / "scripts/lib/fixtures/gnome51-frame-ownership.syscap"
        raw = fixture.read_bytes()
        metadata = json.loads((fixture.with_suffix(".json")).read_text())
        def chunk(_agent, _action, index):
            return {"gnome_trace": {"index": index,
                    "data": base64.b64encode(raw[index * 65536:(index + 1) * 65536]).decode()}}
        for kernel in ([], {"error": "missing live GNU build ID"},
                       {"before": {"release": "one"}, "after": {"release": "two"}, "unchanged": False}):
            with self.subTest(kernel=kernel), tempfile.TemporaryDirectory() as directory:
                out = Path(directory)
                row = dict(metadata, bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest(),
                           capture_writers_after_stop=[], guest_kernel_provenance=kernel)
                with patch.object(host, "guest_probe", side_effect=chunk):
                    with self.assertRaisesRegex(ValueError, "kernel provenance"):
                        host.retain_gnome_trace(None, out, row)
                self.assertEqual((out / "gnome-overview.syscap").read_bytes(), raw)
                self.assertEqual(json.loads((out / "gnome-overview-source.json").read_text()), row)
                self.assertTrue((out / "gnome-overview-marks.json").is_file())


class CaptureClosure(unittest.TestCase):
    def test_writer_closure_required_without_discarding_rejected_capture(self):
        import hashlib
        root = ROOT / "scripts/lib/fixtures"
        raw = (root / "gnome51-frame-ownership.syscap").read_bytes()
        source = json.loads((root / "gnome51-frame-ownership.json").read_text())
        metadata = {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest(),
                    "pid": source["pid"], "gnome_shell_version": "GNOME Shell 51.0"}

        def chunk(agent, action, index):
            self.assertEqual(action, "gnome-trace-read")
            return {"gnome_trace": {"index": index,
                    "data": base64.b64encode(raw[index * 65536:(index + 1) * 65536]).decode()}}

        with tempfile.TemporaryDirectory() as directory, patch.object(host, "guest_probe", chunk):
            out = Path(directory)
            for writers in (None, [{"fd": 80, "flags": os.O_WRONLY}]):
                row = dict(metadata)
                if writers is not None:
                    row["capture_writers_after_stop"] = writers
                with self.subTest(writers=writers), self.assertRaisesRegex(ValueError, "writer did not close"):
                    host.retain_gnome_trace(None, out, row)
                self.assertEqual((out / "gnome-overview.syscap").read_bytes(), raw)
                self.assertTrue((out / "gnome-overview-marks.json").exists())
                self.assertFalse((out / "gnome-overview-frame-ownership.json").exists())
            # This ownership fixture omits the generic event scope. Closing
            # its writer passes the fence but cannot bypass semantic checks.
            with self.assertRaisesRegex(ValueError, "lacks actual event"):
                host.retain_gnome_trace(None, out,
                        dict(metadata, capture_writers_after_stop=[]))


class DiagnosticSourceEvidence(unittest.TestCase):
    def marks(self):
        # Parser-only synthetic records; real package emission is a CI/VM gate.
        return {"marks": [dict(pid=123, monotonic_ns=10, name=name, message=message)
                for name, message in (
                    ("Clutter::FrameClock::dispatch()", ""),
                    ("Clutter::FrameClock::presented()", ""),
                    ("Roost::FrameClock::dispatch-id", "output=Virtual-1 frame=42 dispatch_us=100"),
                    ("Roost::FrameClock::presented-id", "output=Virtual-1 view_frame=42 global_frame=45 presentation_us=90 sequence=98 flags=4 kms_ready_us=101"),
                    ("Roost::KMS::raw-page-flip", "crtc=55 sequence=98 seconds=0 microseconds=90 device=/dev/dri/card1"))]}

    def test_raw_kernel_time_is_retained_without_replacing_early_presentation(self):
        result = sysprof.frame_source_evidence(self.marks(), 123)
        self.assertEqual(result["dispatches"][0]["frame_counter"], 42)
        self.assertEqual(result["presentations"][0]["source_time_us"], 90)
        self.assertEqual(result["kernel_events"][0]["microseconds"], 90)
        self.assertNotIn("latency", result)

    def test_missing_duplicate_foreign_or_malformed_diagnostic_records_fail(self):
        import copy
        for case in ("missing", "duplicate", "foreign", "bad-time", "wrong-output"):
            marks = self.marks()
            if case == "missing":
                marks["marks"].pop()
            elif case == "duplicate":
                marks["marks"].append(copy.deepcopy(marks["marks"][2]))
            elif case == "foreign":
                marks["marks"][2]["pid"] = 124
            elif case == "bad-time":
                marks["marks"][-1]["message"] = marks["marks"][-1]["message"].replace("microseconds=90", "microseconds=1000000")
            else:
                marks["marks"][2]["message"] = marks["marks"][2]["message"].replace("Virtual-1", "Virtual-2")
            with self.subTest(case=case), self.assertRaises(ValueError):
                sysprof.frame_source_evidence(marks, 123)


class RejectedPresentationCapture(unittest.TestCase):
    def capture(self):
        import gzip
        import hashlib
        root = ROOT / "scripts/lib/fixtures"
        metadata = json.loads((root / "gnome51-presentation-before-dispatch.json").read_text())
        raw = gzip.decompress((root / "gnome51-presentation-before-dispatch.syscap.gz").read_bytes())
        self.assertEqual(len(raw), metadata["bytes"])
        self.assertEqual(hashlib.sha256(raw).hexdigest(), metadata["sha256"])
        self.assertEqual(metadata["capture_writers_after_stop"], [])
        return raw, metadata

    def test_complete_closed_actual_capture_skips_and_records_early_presentation(self):
        raw, metadata = self.capture()
        bounds = sysprof.overview_frame_bounds(sysprof.decode(raw), metadata["pid"])
        self.assertEqual(bounds["anomaly_count"], 1)
        evidence = bounds["anomalies"][0]
        self.assertEqual(evidence["kind"], "presentation-precedes-dispatch")
        self.assertEqual(evidence["dispatch"]["monotonic_ns"], 123682891000)
        self.assertEqual(evidence["presentation_lower_ns"], 123677782000)
        self.assertEqual(evidence["presentation_upper_ns"], 123677789000)
        self.assertLess(evidence["presentation_upper_ns"], evidence["dispatch"]["monotonic_ns"])
        self.assertEqual(evidence["kms_ready_ns"], 123684532000)
        self.assertEqual(evidence["swap_count"], 1)

    def test_skip_retains_whole_capture_and_qualified_bounds_with_anomaly(self):
        raw, metadata = self.capture()

        def chunk(agent, action, index):
            self.assertEqual(action, "gnome-trace-read")
            return {"gnome_trace": {"index": index,
                    "data": base64.b64encode(raw[index * 65536:(index + 1) * 65536]).decode()}}

        with tempfile.TemporaryDirectory() as directory, patch.object(host, "guest_probe", chunk):
            out = Path(directory)
            host.retain_gnome_trace(None, out, metadata)
            self.assertEqual((out / "gnome-overview.syscap").read_bytes(), raw)
            self.assertEqual(json.loads((out / "gnome-overview-source.json").read_text()), metadata)
            self.assertTrue((out / "gnome-overview-marks.json").exists())
            owned = json.loads((out / "gnome-overview-frame-ownership.json").read_text())
            self.assertEqual(owned["anomaly_count"], 1)
            self.assertEqual(owned["anomalies"][0]["presentation_upper_ns"], 123677789000)
            self.assertFalse((out / "gnome-overview-frame-rejection.json").exists())


class SysprofCapture(unittest.TestCase):
    def test_actual_gnome51_truncated_scope_names_and_owner(self):
        root = ROOT / "scripts/lib/fixtures"
        provenance = json.loads((root / "gnome51-overview-marks.json").read_text())
        raw = (root / "gnome51-overview-marks.syscap").read_bytes()
        import hashlib
        self.assertEqual(hashlib.sha256(raw).hexdigest(), provenance["sample_sha256"])
        decoded = sysprof.decode(raw)
        counts = sysprof.required_scope_counts(decoded, provenance["pid"])
        self.assertEqual(sorted(counts.values()), [1, 1, 1])
        self.assertEqual(max(len(name) for name in counts), 39)
        self.assertFalse(any(sysprof.required_scope_counts(decoded, provenance["pid"] + 1).values()))

    def ownership_capture(self):
        root = ROOT / "scripts/lib/fixtures"
        provenance = json.loads((root / "gnome51-frame-ownership.json").read_text())
        raw = (root / "gnome51-frame-ownership.syscap").read_bytes()
        import hashlib
        self.assertEqual(hashlib.sha256(raw).hexdigest(), provenance["sample_sha256"])
        return sysprof.decode(raw), provenance["pid"]

    def test_actual_native_trace_balances_owned_frames_and_twenty_inputs(self):
        decoded, pid = self.ownership_capture()
        result = sysprof.overview_frame_bounds(decoded, pid)
        self.assertEqual((result["dispatch_count"], result["presented_count"], result["aborted_count"]), (354, 337, 17))
        self.assertEqual(len(result["inputs"]), 20)
        self.assertEqual(result["pending_count"], 0)
        first = result["inputs"][0]
        self.assertAlmostEqual(first["latency_lower_ms"], 475.063)
        self.assertAlmostEqual(first["latency_upper_ms"], 492.549)
        self.assertTrue(all(row["latency_lower_ms"] <= row["latency_upper_ms"] for row in result["inputs"]))

    def test_kms_feedback_after_kernel_flip_is_bounded_by_notification_not_flip(self):
        import copy
        import re
        decoded, pid = self.ownership_capture()
        before = sysprof.overview_frame_bounds(decoded, pid)
        row = next(row for row in decoded["marks"]
                   if row["name"] == "Clutter::FrameClock::presented()")
        # Reproduce the real second a60 capture: userspace feedback follows
        # the kernel flip, while preceding the notification that carries it.
        ready = row["monotonic_ns"] // 1000 - 1
        row["message"] = re.sub(r"ready at \d+", f"ready at {ready}", row["message"])
        after = sysprof.overview_frame_bounds(decoded, pid)
        self.assertEqual(before["inputs"], after["inputs"], "feedback is not the presentation timestamp")
        for invalid in (0, (row["monotonic_ns"] + row["duration_ns"]) // 1000 + 10):
            bad = copy.deepcopy(decoded)
            changed = next(mark for mark in bad["marks"]
                           if mark["name"] == "Clutter::FrameClock::presented()")
            changed["message"] = re.sub(r"ready at \d+", f"ready at {invalid}", changed["message"])
            with self.subTest(ready=invalid), self.assertRaisesRegex(ValueError, "KMS readiness"):
                sysprof.overview_frame_bounds(bad, pid)

    def test_actual_trace_missing_duplicate_or_wrong_output_completion_fails(self):
        import copy
        for case in ("missing-presented", "missing-ready", "duplicate-presented", "wrong-output", "unknown-description", "missing-swap", "foreign-owner"):
            decoded, pid = self.ownership_capture()
            decoded = copy.deepcopy(decoded)
            marks = decoded["marks"]
            def find(name):
                return next(row for row in marks if row["name"] == name)
            if case == "missing-presented":
                marks.remove(find("Clutter::FrameClock::presented()"))
            elif case == "missing-ready":
                marks.remove(find("Clutter::FrameClock::ready()"))
            elif case == "duplicate-presented":
                marks.append(copy.deepcopy(find("Clutter::FrameClock::presented()")))
            elif case == "wrong-output":
                find("Clutter::FrameClock::ready()")["message"] = "Virtual-2"
            elif case == "unknown-description":
                find("Clutter::FrameClock::presented()")["message"] = "no kernel time"
            elif case == "missing-swap":
                marks.remove(find("Meta::StageImpl::swap_framebuffer()"))
            else:
                pid += 1
            with self.subTest(case=case), self.assertRaises(ValueError):
                sysprof.overview_frame_bounds(decoded, pid)

    def test_missing_extra_overlapping_or_coalesced_input_is_not_a_latency(self):
        import copy
        for case in ("missing", "extra", "overlap", "coalesced"):
            decoded, pid = self.ownership_capture()
            marks = decoded["marks"]
            inputs = [row for row in marks if row["name"] == "Meta::Display::handle_event()" and row["message"] == "key-release"]
            if case == "missing":
                marks.remove(inputs[0])
            elif case == "extra":
                marks.append(copy.deepcopy(inputs[0]))
            elif case == "overlap":
                dispatch = next(row for row in marks if row["name"] == "Clutter::FrameClock::dispatch()")
                inputs[0]["monotonic_ns"] = dispatch["monotonic_ns"]
            else:
                inputs[1]["monotonic_ns"] = inputs[0]["monotonic_ns"] + inputs[0]["duration_ns"] + 1000
            with self.subTest(case=case), self.assertRaises(ValueError):
                sysprof.overview_frame_bounds(decoded, pid)

    def capture(self):
        # Produced by the real libsysprof-capture writer, not by this decoder.
        return (ROOT / "scripts/lib/fixtures/sysprof-mark.syscap").read_bytes()

    def test_native_writer_mark(self):
        result = sysprof.decode(self.capture())
        self.assertEqual(result["frame_count"], 1)
        self.assertEqual(result["marks"], [dict(cpu=2, pid=4242, monotonic_ns=123456789,
                                               duration_ns=2000, group="Clutter",
                                               name="Clutter::FrameClock::presented()",
                                               message="presentation was 5 µs earlier")])

    def test_partial_headers_frames_and_trailing_bytes_fail(self):
        raw = self.capture()
        for length in (0, 255, 256, 260, len(raw) - 1):
            with self.subTest(length=length), self.assertRaises(ValueError):
                sysprof.decode(raw[:length])
        with self.assertRaises(ValueError):
            sysprof.decode(raw + b"x")

    def test_bad_header_duration_strings_and_lengths_fail(self):
        import struct
        for offset, replacement in ((0, b"BAD!"), (4, b"\2"), (5, b"\0"),
                                    (256, struct.pack("<H", 25)),
                                    (280, struct.pack("<q", -1)),
                                    (288, b"x" * 24), (312, b"x" * 40),
                                    (352, b"\xff\0")):
            raw = bytearray(self.capture())
            raw[offset:offset + len(replacement)] = replacement
            with self.subTest(offset=offset), self.assertRaises(ValueError):
                sysprof.decode(raw)

    def test_unknown_frame_is_structurally_checked_and_skipped(self):
        import struct
        raw = self.capture() + struct.pack("<HhiqII", 24, -1, 4242, 100, 254, 0)
        result = sysprof.decode(raw)
        self.assertEqual(result["frame_count"], 2)
        self.assertEqual(len(result["marks"]), 1)


class Accounting(unittest.TestCase):
    def globals_serial(self, raw, records=None):
        records = guest.global_records(raw, "wayland-0") if records is None else records
        return "\n".join("journal prefix roost-perf-globals: " + json.dumps(row)
                         for row in records)

    def test_globals_preserve_multichunk_raw_bytes(self):
        raw = (b"interface: 'wl_compositor', version: 6\n"
               b"interface: 'xdg_wm_base', version: 6\n" + b"a" * 7000)
        text, meta = host.raw_globals(self.globals_serial(raw))
        self.assertEqual(text.encode(), raw)
        self.assertEqual(meta["socket"], "wayland-0")

    def test_globals_partial_duplicate_and_corrupt_captures_fail(self):
        raw = (b"interface: 'wl_compositor'\ninterface: 'xdg_wm_base'\n" + b"a" * 7000)
        rows = guest.global_records(raw, "wayland-0")
        for broken in (rows[:-1], rows + [rows[0]],
                       [dict(row, sha256="0" * 64) for row in rows]):
            with self.subTest(broken=broken[0]["sha256"]), self.assertRaises(ValueError):
                host.raw_globals(self.globals_serial(raw, broken))

    def test_globals_missing_and_private_socket_are_not_desktop_evidence(self):
        with self.assertRaises(ValueError):
            host.raw_globals("")
        with self.assertRaises(ValueError):
            host.raw_globals(self.globals_serial(b"interface: 'zwp_input_method_manager_v2'"))

    def presentation(self):
        return {"clock_id": 1, "commits": 242, "discarded": 1, "pending": 1,
                "frames": [dict(commit=i+1, presented_ns=(i+1)*16000000,
                                refresh_ns=16000000, sequence=i, flags=1)
                           for i in range(240)]}

    def test_presentation_requires_complete_monotonic_nonduplicate_feedback(self):
        for case in ("missing", "duplicate", "backwards", "unbalanced", "boolean"):
            trace = self.presentation()
            if case == "missing":
                trace["frames"].pop()
            elif case == "duplicate":
                trace["frames"][1]["commit"] = 1
            elif case == "backwards":
                trace["frames"][1]["presented_ns"] = 1
            elif case == "unbalanced":
                trace["discarded"] = 0
            elif case == "boolean":
                trace["frames"][1]["refresh_ns"] = True
            with self.subTest(case=case), self.assertRaises(ValueError):
                host.presentation_summary(trace)

    def test_software_presentation_does_not_fabricate_hardware_or_refresh(self):
        trace = self.presentation()
        for row in trace["frames"]:
            row["refresh_ns"] = 0
            row["flags"] = 0
        result = host.presentation_summary(trace)
        self.assertEqual(result["interval_ms"]["count"], 239)
        self.assertEqual(result["interval_ms"]["p99"], 16)
        self.assertEqual(result["reported_refresh_ms"], {"count": 0})
        self.assertEqual(result["unknown_refresh_count"], 240)
        self.assertEqual(result["flag_counts"], dict(vsync=0, hw_clock=0, hw_completion=0, zero_copy=0))
        self.assertEqual(result["discarded_count"], 1)

    def process(self, root, pid, ppid, uid, pss, start):
        path = root / str(pid)
        path.mkdir()
        (path / "status").write_text(f"Uid: {uid} {uid} {uid} {uid}\n")
        fields = ["0"] * 50
        fields[0] = "S";fields[1] = str(ppid)
        fields[11] = "30";fields[12] = "20";fields[19] = str(start)
        (path / "stat").write_text(f"{pid} (command ) with spaces) " + " ".join(fields))
        (path / "smaps_rollup").write_text(f"Pss: {pss} kB\nRss: {pss*2} kB\n")
        (path / "fd").mkdir()
        (path / "fd/0").touch()
        return path

    def test_scope_excludes_other_users_observer_and_descendants(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.process(root, 1, 0, 1000, 10, 55)
            self.process(root, 2, 1, 1000, 20, 66)
            self.process(root, 3, 2, 1000, 30, 77)
            self.process(root, 4, 0, 1000, 40, 88)
            self.process(root, 5, 0, 1001, 50, 99)
            result = guest.sample(1000, root, observer=1)
            self.assertEqual(result["pss_kib"], 40)
            self.assertEqual(result["process_count"], 1)
            self.assertEqual(result["fds"], 1)
            self.assertEqual(result["processes"][0]["cpu_ticks"], 50)
            self.assertEqual(result["processes"][0]["start_ticks"], 88)

    def test_memory_read_failure_is_reported_instead_of_understating_pss(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            (path / "smaps_rollup").unlink()
            result = guest.sample(1000, root)
            self.assertEqual(result["unreadable"], [{"pid":1,"error":"FileNotFoundError"}])

    def test_drm_duplicate_handles_and_shared_process_clients_are_counted_once(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid in (1, 2):
                path = self.process(root, pid, 0, 1000, 10, 55)
                (path / "fdinfo").mkdir()
                for fd in (0, 1):
                    (path / f"fdinfo/{fd}").write_text(
                        "drm-driver: test\ndrm-pdev: 0000:00:02.0\ndrm-client-id: 7\n"
                        "drm-resident-vram: 2 MiB\ndrm-shared-vram: 4 KiB\n"
                        "drm-total-memory: 128\nprivate-text: must not be retained\n")
            result = guest.sample(1000, root)
            self.assertEqual(result["drm_memory"]["status"], "available")
            self.assertEqual(len(result["drm_memory"]["clients"]), 1)
            client = result["drm_memory"]["clients"][0]
            self.assertEqual(client["memory_bytes"], {"drm-resident-vram": 2097152,
                                                     "drm-shared-vram": 4096,
                                                     "drm-total-memory": 128})
            self.assertNotIn("private-text", json.dumps(result))
            summary = host.drm_memory_summary([result])
            self.assertEqual(summary["sample_status_counts"], {"available": 1})
            self.assertEqual(next(c for c in summary["client_accounted_bytes"]
                                  if c["counter"] == "drm-resident-vram")["distribution"]["p50"], 2097152)

    def test_drm_unsupported_and_invalid_counters_are_not_measured_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            absent = guest.sample(1000, root)
            self.assertEqual(absent["drm_memory"]["status"], "unavailable")
            (path / "fdinfo").mkdir()
            (path / "fdinfo/0").write_text(
                "drm-driver: test\ndrm-client-id: 1\ndrm-memory-vram: -2 KiB\n")
            invalid = guest.sample(1000, root)
            self.assertEqual(invalid["drm_memory"]["status"], "incomplete")
            summary = host.drm_memory_summary([absent, invalid])
            self.assertEqual(summary["sample_status_counts"], {"unavailable": 1, "incomplete": 1})
            self.assertEqual(summary["client_accounted_bytes"], [])

    def test_drm_measured_zero_and_separate_devices_preserve_their_meaning(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            (path / "fdinfo").mkdir()
            for fd in (0, 1):
                (path / f"fdinfo/{fd}").write_text(
                    f"drm-driver: test\ndrm-pdev: 0000:00:0{fd}.0\ndrm-client-id: 1\n"
                    "drm-memory-vram: 0 KiB\n")
            result = guest.sample(1000, root)
            self.assertEqual(result["drm_memory"]["status"], "available")
            self.assertEqual(len(result["drm_memory"]["clients"]), 2)
            summary = host.drm_memory_summary([result])
            self.assertEqual(len(summary["client_accounted_bytes"]), 2)
            self.assertTrue(all(c["distribution"]["p50"] == 0
                                for c in summary["client_accounted_bytes"]))

    def test_zombie_is_departed_even_before_parent_reaps_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            stat = (path / "stat").read_text().replace(") S ", ") Z ")
            (path / "stat").write_text(stat)
            (path / "smaps_rollup").unlink()
            result = guest.sample(1000, root)
            self.assertEqual(result["unreadable"], [])
            self.assertEqual(result["process_count"], 0)

    def test_journal_prefix_and_truncated_serial_do_not_corrupt_valid_samples(self):
        result = host.raw_samples('prefix roost-perf-sample: {"pss_kib":7}\nroost-perf-sample: {bad\n')
        self.assertEqual(result, [{"pss_kib":7}])

    def test_small_sample_percentiles_keep_the_extreme_tail(self):
        result = host.distribution([1, 9, 2, 3])
        self.assertEqual(result["count"], 4)
        self.assertEqual(result["p95"], 9)
        self.assertEqual(result["p99"], 9)
        self.assertEqual(host.distribution([]), {"count":0})

    def test_idle_cpu_excludes_intervals_crossing_either_guest_clock_boundary(self):
        rows = [{"cpu_interval_start_boottime_s": i, "boottime_s": i+1,
                 "cpu_observed_percent": 2} for i in range(100, 120)]
        rows += [{"cpu_interval_start_boottime_s": 99, "boottime_s": 101,
                  "cpu_observed_percent": 999},
                 {"cpu_interval_start_boottime_s": 119, "boottime_s": 121,
                  "cpu_observed_percent": 999},
                 {"cpu_interval_start_boottime_s": None, "boottime_s": 100,
                  "cpu_observed_percent": None}]
        result = host.idle_cpu(rows, 100, 120)
        self.assertEqual(result["count"], 20)
        self.assertEqual(result["max"], 2)
        with self.assertRaisesRegex(ValueError, "fewer than 20"):
            host.idle_cpu(rows, 101, 120)
        with self.assertRaisesRegex(ValueError, "invalid guest idle"):
            host.idle_cpu(rows, 120, 100)

    def test_guest_phase_probe_requires_executed_complete_valid_output(self):
        class Agent:
            def __init__(self, result, **flags):
                self.commands = []
                self.status = {"exited": True, "exitcode": 0,
                               "out-data": base64.b64encode(json.dumps(result).encode()).decode(),
                               **flags}

            def command(self, name, **arguments):
                self.commands.append((name, arguments))
                return {"pid": 7} if name == "guest-exec" else self.status

        agent = Agent({"boottime_s": 100, "notification_id": 9})
        self.assertEqual(host.guest_probe(agent, "notify", 3)["notification_id"], 9)
        self.assertEqual(agent.commands[0][1]["path"], "/usr/libexec/roost-perf-phase")
        self.assertEqual(agent.commands[0][1]["arg"], ["notify", "--index", "3"])
        error_agent = Agent({}, exitcode=1,
                            **{"err-data": base64.b64encode(b"module filename rejected: virtio-gpu.ko.zst").decode()})
        with self.assertRaisesRegex(RuntimeError, "virtio-gpu.ko.zst"):
            host.guest_probe(error_agent, "gnome-trace-start")
        oversized = Agent({}, exitcode=1,
                          **{"err-data": base64.b64encode(b"x" * 100000).decode()})
        with self.assertRaises(RuntimeError) as error:
            host.guest_probe(oversized, "gnome-trace-start")
        self.assertLess(len(str(error.exception)), 4500)
        invalid = Agent({}, exitcode=1, **{"err-data": "not base64!"})
        with self.assertRaisesRegex(RuntimeError, "invalid guest stderr encoding"):
            host.guest_probe(invalid, "gnome-trace-start")
        for flags in ({"exitcode": 1}, {"out-truncated": True}, {"err-truncated": True}):
            with self.subTest(flags=flags), self.assertRaisesRegex(RuntimeError, "failed"):
                host.guest_probe(Agent({"boottime_s": 100}, **flags), "clock")
        for clock in (None, True, -1, float("nan")):
            with self.subTest(clock=clock), self.assertRaisesRegex(ValueError, "invalid guest clock"):
                host.guest_probe(Agent({"boottime_s": clock}), "clock")
        for identity in (0, True, None, 0x100000000):
            with self.subTest(identity=identity), self.assertRaisesRegex(ValueError, "notification ID"):
                host.guest_probe(Agent({"boottime_s": 100, "notification_id": identity}), "notify", 0)
        for action, index in (("exec", None), ("notify", -1), ("notify", 10), ("notify", True)):
            with self.assertRaises(ValueError):
                host.guest_probe(agent, action, index)




class CaptureAcquisition(unittest.TestCase):
    def test_real_kernel_writer_handles_exclude_readers_and_other_inodes(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "capture"
            target.write_bytes(b"raw capture fixture")
            other = Path(directory) / "other"
            other.write_bytes(b"other writer")
            with target.open("rb") as reader, target.open("ab") as writer, other.open("ab"):
                metadata = os.fstat(reader.fileno())
                observed = phase.capture_writers(os.getpid(), metadata)
                self.assertEqual([row["fd"] for row in observed], [writer.fileno()])
                writer.close()
                self.assertEqual(phase.capture_writers(os.getpid(), metadata), [])


class PinnedBaselinePackageGuards(unittest.TestCase):
    """Execute the actual Containerfile shell, including its AND-list context."""

    def test_preflight_failure_prevents_any_package_install(self):
        source = (ROOT / "packaging/marlin/Containerfile").read_text().replace("\\\n", "")
        script = source.split("RUN set -eux;", 1)[1].split("&& pacman -U", 1)[0] + " && :"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "share").mkdir()
            (root / "bin").mkdir()
            inventory = root / "share/roost-perf-baseline-packages.txt"
            manifest = root / "share/roost-perf-baseline-critical.sha256"
            critical = root / "critical"
            marker = root / "package-install-started"
            original = b"immutable baseline fixture"
            manifest.write_text(f"{hashlib.sha256(original).hexdigest()}  {critical}\n")
            pacman = root / "bin/pacman"
            pacman.write_text('#!/bin/sh\nif test "$1" = -Q; then printf "%s\\n" '
                              '"$TEST_MUTTER_PIN"; else touch "$TEST_PACKAGE_MUTATION"; fi\n')
            pacman.chmod(0o755)
            script = script.replace("/usr/share", str(root / "share"))
            script = script.replace("/tmp/roost-", str(root / "roost-"))
            for name, has_inventory, pin, corrupt, accepted in (
                ("valid", True, "mutter 51.0-1.3", False, True),
                ("missing-inventory", False, "mutter 51.0-1.3", False, False),
                ("wrong-pin", True, "mutter 51.0-2", False, False),
                ("changed-critical-file", True, "mutter 51.0-1.3", True, False),
            ):
                with self.subTest(name=name):
                    inventory.unlink(missing_ok=True)
                    marker.unlink(missing_ok=True)
                    if has_inventory:
                        inventory.write_text("mutter 51.0-1.3\n")
                    critical.write_bytes(b"changed" if corrupt else original)
                    environment = dict(os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}",
                                       PERF_PINNED_BASELINE="true", TEST_MUTTER_PIN=pin,
                                       TEST_PACKAGE_MUTATION=str(marker))
                    result = subprocess.run(["bash", "-e", "-c", script], env=environment,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, accepted, result.stderr)
                    self.assertEqual(marker.exists(), accepted)

    def test_post_install_rejects_changed_missing_or_corrupt_baseline(self):
        source = (ROOT / "packaging/marlin/Containerfile").read_text().replace("\\\n", "")
        beginning = 'if test "$PERF_PINNED_BASELINE" = true; then'
        script = beginning + source.split(beginning, 1)[1].split("\n\nLABEL", 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "share").mkdir()
            (root / "bin").mkdir()
            (root / "share/roost-perf-baseline-packages.txt").write_text(
                "linux 7.2.9.arch1-1\nmutter 51.0-1.3\n")
            critical = root / "critical"
            original = b"immutable baseline fixture"
            (root / "share/roost-perf-baseline-critical.sha256").write_text(
                f"{hashlib.sha256(original).hexdigest()}  {critical}\n")
            pacman = root / "bin/pacman"
            pacman.write_text('#!/bin/sh\ncat "$TEST_PACKAGE_INVENTORY"\n')
            pacman.chmod(0o755)
            script = script.replace("/usr/share", str(root / "share"))
            script = script.replace("/tmp/roost-", str(root / "roost-"))
            for name, body, corrupt, accepted in (
                ("unchanged", "linux 7.2.9.arch1-1\nmutter 51.0-1.3\n", False, True),
                ("new-roost", "linux 7.2.9.arch1-1\nmutter 51.0-1.3\nroost 1.0-1\n", False, True),
                ("kernel-upgrade", "linux 7.2.10.arch1-1\nmutter 51.0-1.3\n", False, False),
                ("mutter-replacement", "linux 7.2.9.arch1-1\nmutter 51.0-2\n", False, False),
                ("missing", "linux 7.2.9.arch1-1\n", False, False),
                ("critical-corruption", "linux 7.2.9.arch1-1\nmutter 51.0-1.3\n", True, False),
            ):
                with self.subTest(name=name):
                    inventory = root / "inventory"
                    inventory.write_text(body)
                    critical.write_bytes(b"changed" if corrupt else original)
                    environment = dict(os.environ, PATH=f"{root / 'bin'}:{os.environ['PATH']}",
                                       PERF_PINNED_BASELINE="true", TEST_PACKAGE_INVENTORY=str(inventory))
                    result = subprocess.run(["bash", "-e", "-c", script], env=environment,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode == 0, accepted, result.stderr)

    def test_all_baseline_containerfile_run_blocks_have_valid_shell_syntax(self):
        for name in ("packaging/marlin/Containerfile", "packaging/marlin/perf/Containerfile",
                     "packaging/marlin/perf/Containerfile.baseline"):
            for line in (ROOT / name).read_text().replace("\\\n", "").splitlines():
                if line.startswith("RUN "):
                    with self.subTest(containerfile=name, run=line):
                        result = subprocess.run(["bash", "-n"], input=line[4:], text=True,
                                                capture_output=True)
                        self.assertEqual(result.returncode, 0, result.stderr)

if __name__ == "__main__":
    unittest.main()
