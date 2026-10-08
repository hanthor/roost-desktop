#!/usr/bin/env python3
"""Reject unowned/forged acquisition boundaries without changing clock bounds."""
import copy
import importlib.util
from pathlib import Path
import unittest

ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('drained_decoder',ROOT/'scripts/lib/gnome_sysprof.py')
decoder=importlib.util.module_from_spec(spec);spec.loader.exec_module(decoder)


class Boundary(unittest.TestCase):
    def fixture(self):
        def mark(name,stamp,message):
            return {'pid':42,'monotonic_ns':stamp,'duration_ns':0,'name':name,'message':message}
        rows=[mark('Roost::CaptureBoundary',20_000_000,
                   'output=Virtual-1 clock=0xabcd next=5 pending=0 depth=0 dispatched=0 state=scheduled requested_us=10000 enabled_us=20000 samples=2'),
              mark('Clutter::FrameClock::dispatch()',21_000_000,'Virtual-1'),
              mark('Roost::FrameClock::dispatch-id',21_000_100,'output=Virtual-1 frame=5 dispatch_us=21000'),
              mark('Clutter::FrameClock::presented()',22_000_000,'Virtual-1'),
              mark('Roost::FrameClock::presented-id',22_000_100,'output=Virtual-1 view_frame=5 global_frame=100 presentation_us=21900 sequence=22 flags=5 kms_ready_us=21500'),
              mark('Roost::KMS::raw-page-flip',22_000_200,'crtc=39 sequence=22 seconds=0 microseconds=21900 device=/dev/dri/card1')]
        return {'marks':rows},{'pid':42,'drained_start_required':True,'start_requested_monotonic_ns':10_000_000,'started_monotonic_ns':20_500_000}
    def test_real_boundary_source_counter_and_first_dispatch_accepted(self):
        d,m=self.fixture();r=decoder.capture_boundary_evidence(d,m)
        self.assertEqual(r['next_counter'],5);self.assertEqual(r['first_source_dispatch']['frame_counter'],5)
    def test_missing_duplicate_or_foreign_owner_boundary_rejected(self):
        for mutation in ('missing','duplicate','foreign'):
            d,m=self.fixture()
            if mutation=='missing':d['marks'].pop(0)
            elif mutation=='duplicate':d['marks'].append(copy.deepcopy(d['marks'][0]))
            else:d['marks'][0]['pid']=43
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_busy_slots_depth_dispatched_and_wrong_output_rejected(self):
        for before,after in [('pending=0','pending=1'),('depth=0','depth=1'),('dispatched=0','dispatched=1'),
                             ('state=scheduled','state=dispatched-one'),('Virtual-1','Virtual-2'),('clock=0xabcd','clock=0x0')]:
            d,m=self.fixture();d['marks'][0]['message']=d['marks'][0]['message'].replace(before,after)
            with self.subTest(after=after),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_actual_wait_deadline_sample_bound_and_clock_order_rejected(self):
        for before,after in [('samples=2','samples=0'),('samples=2','samples=5001'),
                             ('requested_us=10000','requested_us=20001'),('enabled_us=20000','enabled_us=20001'),
                             ('enabled_us=20000','enabled_us=5010000')]:
            d,m=self.fixture();d['marks'][0]['message']=d['marks'][0]['message'].replace(before,after)
            with self.subTest(after=after),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_boundary_after_acknowledgement_or_before_request_rejected(self):
        for key,value in [('started_monotonic_ns',19_000_000),('start_requested_monotonic_ns',21_000_000),('drained_start_required',False)]:
            d,m=self.fixture();m[key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_original_leading_unowned_completion_is_not_trimmed(self):
        d,m=self.fixture();bad=dict(d['marks'][3],monotonic_ns=20_100_000);d['marks'].insert(1,bad)
        with self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
        self.assertIn(bad,d['marks'])
    def test_wrong_next_source_counter_missing_source_or_preboundary_dispatch_rejected(self):
        for mutation in ('counter','source','early'):
            d,m=self.fixture()
            if mutation=='counter':d['marks'][2]['message']=d['marks'][2]['message'].replace('frame=5','frame=6')
            elif mutation=='source':d['marks'].pop(2)
            else:d['marks'][1]['monotonic_ns']=19_000_000
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):decoder.capture_boundary_evidence(d,m)
    def test_boundary_does_not_replace_strict_owned_frame_clock_checks(self):
        d,m=self.fixture();decoder.capture_boundary_evidence(d,m)
        with self.assertRaises(ValueError):decoder.overview_frame_bounds(d,m['pid'])



class StopBoundary(unittest.TestCase):
    def fixture(self):
        from gnome_stop_boundary import capture_stop_boundary_evidence
        self.validate = capture_stop_boundary_evidence
        decoded, metadata = Boundary().fixture()
        metadata.update(drained_stop_required=True, stop_requested_monotonic_ns=23_000_000,
                        stopped_monotonic_ns=25_500_000)
        decoded['marks'].append({'pid':42,'monotonic_ns':25_000_000,'duration_ns':0,
                                'name':'Roost::CaptureStopBoundary',
                                'message':'output=Virtual-1 clock=0xabcd next=6 pending=0 depth=0 dispatched=0 state=idle requested_us=23000 closing_us=25000 samples=2'})
        return decoded, metadata
    def test_actual_original_closed_clock_accepted_without_frame_changes(self):
        d,m=self.fixture();original=copy.deepcopy(d);result=self.validate(d,m)
        self.assertEqual(result['next_counter'],6);self.assertEqual(result['completion_count'],1)
        self.assertEqual(d,original)
    def test_missing_duplicate_foreign_or_wrong_clock_stop_refused(self):
        for mode in ('missing','duplicate','foreign','clock'):
            d,m=self.fixture()
            if mode=='missing':d['marks'].pop()
            elif mode=='duplicate':d['marks'].append(copy.deepcopy(d['marks'][-1]))
            elif mode=='foreign':d['marks'][-1]['pid']=43
            else:d['marks'][-1]['message']=d['marks'][-1]['message'].replace('0xabcd','0xeeee')
            with self.subTest(mode=mode),self.assertRaises(ValueError):self.validate(d,m)
    def test_pending_dispatch_depth_and_wrong_counter_refused(self):
        for old,new in [('pending=0','pending=1'),('depth=0','depth=1'),('dispatched=0','dispatched=1'),
                        ('state=idle','state=dispatched-one'),('next=6','next=5'),('Virtual-1','Virtual-2')]:
            d,m=self.fixture();d['marks'][-1]['message']=d['marks'][-1]['message'].replace(old,new)
            with self.subTest(new=new),self.assertRaises(ValueError):self.validate(d,m)
    def test_deadline_sample_count_or_request_return_order_refused(self):
        for old,new in [('samples=2','samples=0'),('samples=2','samples=5001'),
                        ('requested_us=23000','requested_us=1'),('closing_us=25000','closing_us=5023000')]:
            d,m=self.fixture();d['marks'][-1]['message']=d['marks'][-1]['message'].replace(old,new)
            with self.subTest(new=new),self.assertRaises(ValueError):self.validate(d,m)
        for key,value in [('stop_requested_monotonic_ns',None),('stop_requested_monotonic_ns',True),
                          ('stop_requested_monotonic_ns',20_000_000),('stopped_monotonic_ns',24_000_000),
                          ('drained_stop_required',False)]:
            d,m=self.fixture();m[key]=value
            with self.subTest(key=key),self.assertRaises(ValueError):self.validate(d,m)
    def test_scope_crossing_stop_boundary_or_missing_completion_refused(self):
        d,m=self.fixture();d['marks'][3]['duration_ns']=4_000_000
        with self.assertRaises(ValueError):self.validate(d,m)
        d,m=self.fixture();d['marks']=[row for row in d['marks'] if row['name'] not in
            ('Clutter::FrameClock::presented()','Roost::FrameClock::presented-id')]
        with self.assertRaises(ValueError):self.validate(d,m)
    def test_stop_boundary_does_not_relax_owned_frame_decoder(self):
        d,m=self.fixture();self.validate(d,m)
        with self.assertRaises(ValueError):decoder.overview_frame_bounds(d,m['pid'])


class FailedCapture(unittest.TestCase):
    def fixture(self):
        from gnome_failed_capture import failed_source
        self.validate=failed_source
        modules={name:{'package':'mutter 51.0-1.6','sha256':'a'*64,'bytes':100,
                       'uid':0,'device':1,'inode':2,'path':'/usr/lib/'+{'cogl':'libmutter-cogl-51','clutter':'libmutter-clutter-51','core':'libmutter-51'}[name]+'.so.0.0.0'}
                 for name in ('cogl','clutter','core')}
        source={'uid':1000,'pid':42,'owner':':1.10','process_start':123,
                'gnome_shell_version':'GNOME Shell 51.0','mutter_mapped_libraries':modules,
                'mutter_cogl_library':modules['cogl'],'drained_start_required':True,
                'drained_stop_required':True,'start_requested_monotonic_ns':100,
                'started_monotonic_ns':200,'capture_identity':{'device':1,'inode':2,'uid':1000}}
        failure={'trace_failure':{'schema':1,'action':'stop','stage':'stop-call','error_class':'GLibError',
                 'principal':{'uid':1000,'pid':42,'owner':':1.10'},'drained_failure':'stop-timeout',
                 'glib':{'domain':'g-dbus-error-quark','code':20,'remote_name':'org.freedesktop.DBus.Error.TimedOut'}}}
        return {'source':source,'failure':failure,'stop_requested_monotonic_ns':300},modules
    def test_only_original_finite_failed_stop_accepts_no_qualification(self):
        saved,modules=self.fixture();source=self.validate(saved,1000,':1.10',42,modules,123)
        self.assertEqual(source,saved['source']);self.assertNotIn('qualified',source)
    def test_private_extra_wrong_principal_epoch_module_or_file_binding_refused(self):
        for mode in ('private','bool','owner','start','package','capture','failure','timestamp'):
            saved,modules=self.fixture()
            if mode=='private':saved['source']['PRIVATE']='secret'
            elif mode=='bool':saved['source']['pid']=True
            elif mode=='owner':saved['source']['owner']=':1.11'
            elif mode=='start':saved['source']['process_start']=124
            elif mode=='package':modules['cogl']['package']='mutter 51.0-1.5'
            elif mode=='capture':saved['source']['capture_identity']['uid']=0
            elif mode=='failure':saved['failure']['trace_failure']['drained_failure']='trace-enable'
            else:saved['stop_requested_monotonic_ns']=True
            with self.subTest(mode=mode),self.assertRaises(ValueError):self.validate(saved,1000,':1.10',42,modules,123)
    def test_unknown_or_malformed_failed_run_envelope_refused(self):
        for mutate in (lambda s:s.update(PRIVATE='secret'),lambda s:s.update(source=[]),
                       lambda s:s.update(failure={}),lambda s:s.update(source=None)):
            saved,modules=self.fixture();mutate(saved)
            with self.assertRaises(ValueError):self.validate(saved,1000,':1.10',42,modules,123)
    def test_real_original_raw_fd_sha_and_inodes_are_retained(self):
        import os,tempfile,hashlib
        from gnome_failed_capture import read_raw,identity
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'capture';raw=b'controlled-rejected-raw'*20;path.write_bytes(raw);path.chmod(0o600)
            fd=os.open(path,os.O_RDONLY)
            try:
                result=read_raw(fd,path,identity(os.fstat(fd)),os.getuid())
                self.assertEqual(result['bytes'],len(raw));self.assertEqual(result['sha256'],hashlib.sha256(raw).hexdigest())
            finally:os.close(fd)
    def test_replaced_original_raw_small_file_or_exposed_mode_refused(self):
        import os,tempfile
        from gnome_failed_capture import read_raw,identity
        for mode in ('replacement','small','permissions','wronginode'):
            with tempfile.TemporaryDirectory() as directory:
                path=Path(directory)/'capture';path.write_bytes(b'x'*256);path.chmod(0o600)
                fd=os.open(path,os.O_RDONLY);original=identity(os.fstat(fd))
                try:
                    if mode=='replacement':
                        new=Path(directory)/'new';new.write_bytes(b'x'*256);new.chmod(0o600);new.replace(path)
                    elif mode=='small':path.write_bytes(b'x'*255)
                    elif mode=='permissions':path.chmod(0o644)
                    else:original['inode']+=1
                    with self.subTest(mode=mode),self.assertRaises(ValueError):read_raw(fd,path,original,os.getuid())
                finally:os.close(fd)
    def test_bounded_marker_exclusive_duplicate_and_symlink_refusals(self):
        import os,tempfile
        from gnome_failed_capture import write_marker,read_marker
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'marker';write_marker(path,{'known':1},os.getuid())
            self.assertEqual(read_marker(path,os.getuid()),{'known':1})
            with self.assertRaises(FileExistsError):write_marker(path,{'known':2},os.getuid())
            path.write_text('{"known":1,"known":2}')
            with self.assertRaises(ValueError):read_marker(path,os.getuid())
            path.write_text('x'*8193)
            with self.assertRaises(ValueError):read_marker(path,os.getuid())
            link=Path(directory)/'link';link.symlink_to(path)
            with self.assertRaises(OSError):read_marker(link,os.getuid())
    def test_marker_read_refuses_named_replacement_and_mode_change(self):
        import os,tempfile
        from unittest import mock
        from gnome_failed_capture import write_marker,read_marker
        for mode in ('replace','mode'):
            with tempfile.TemporaryDirectory() as directory:
                path=Path(directory)/'marker';write_marker(path,{'known':1},os.getuid())
                new=Path(directory)/'new';write_marker(new,{'known':1},os.getuid())
                actual=os.read
                def changed(fd,count):
                    raw=actual(fd,count)
                    if mode=='replace' and new.exists():new.replace(path)
                    else:path.chmod(0o644)
                    return raw
                with mock.patch.object(os,'read',side_effect=changed),self.assertRaises(ValueError):read_marker(path,os.getuid())
    def test_actual_static_stop_failures_are_finite_private_safe_and_stage_bound(self):
        import perf_trace_diagnostics as diag
        class Error(Exception):domain='g-dbus-error-quark';code=0
        patch=(ROOT/'packaging/marlin/perf/mutter-profiler/drained-capture-start.patch').read_text()
        for message,label in diag.STOP_DRAINED_FAILURES.items():
            self.assertIn('"'+message+'"',patch)
            remote='org.freedesktop.DBus.Error.'+('TimedOut' if label=='stop-timeout' else 'Failed')
            context=diag.Context('stop');context.stage='stop-call';error=Error('PRIVATE');error.message='GDBus.Error:'+remote+': '+message
            value=context.failure(error,Error,lambda _:remote);raw=diag.encode(value,'stop')
            self.assertEqual(diag.decode(raw,'stop')['trace_failure']['drained_failure'],label)
            self.assertNotIn(b'PRIVATE',raw);self.assertNotIn(message.encode(),raw)
            error.message += ' PRIVATE'
            self.assertNotIn('drained_failure',context.failure(error,Error,lambda _:remote)['trace_failure'])


class FailedReceipt(unittest.TestCase):
    def receipt(self):
        from gnome_failed_capture import validate_receipt,validate_chunk
        self.validate=validate_receipt;self.chunk=validate_chunk
        saved,modules=FailedCapture().fixture()
        return dict(saved['source'],qualified=False,failure=saved['failure'],
                    stop_requested_monotonic_ns=300,failed_stop_observed_monotonic_ns=400,
                    bytes=256,sha256='a'*64,capture_mtime_ns=1,capture_writers_at_failure=[],
                    capture_writers_after_failure=[],capture_writer_close_wait_ns=10,writer_closure_observed=True)
    def test_exact_original_finite_closed_receipt_passes(self):
        value=self.receipt();self.assertEqual(self.validate(value),value)
    def test_private_bool_wrong_original_and_unclosed_receipts_refused(self):
        for key,val in [('PRIVATE','secret'),('writer_closure_observed',False),('writer_closure_observed',1),
                        ('capture_writers_after_failure',[{'fd':3,'flags':1}]),('bytes',True),
                        ('failed_stop_observed_monotonic_ns',299),('capture_writer_close_wait_ns',True)]:
            value=self.receipt();value[key]=val
            with self.subTest(key=key),self.assertRaises(ValueError):self.validate(value)
        value=self.receipt();value['mutter_mapped_libraries']['cogl']['PRIVATE']='secret'
        with self.assertRaises(ValueError):self.validate(value)
        value=self.receipt();value['capture_writers_at_failure']=[{'fd':3,'flags':1}]*65
        with self.assertRaises(ValueError):self.validate(value)
    def test_exact_chunk_bools_alias_data_and_forged_identity_refused(self):
        import base64
        value=self.receipt();row={'index':0,'data':base64.b64encode(b'x'*256).decode(),'capture_identity':value['capture_identity']}
        self.assertEqual(self.chunk(row,0,value),b'x'*256)
        for key,val in [('index',False),('data',True),('PRIVATE','secret'),('data','a'*90000),
                        ('capture_identity',{'device':True,'inode':2,'uid':1000})]:
            bad=copy.deepcopy(row);bad[key]=val
            with self.subTest(key=key),self.assertRaises(ValueError):self.chunk(bad,0,value)


class WriterClosure(unittest.TestCase):
    def test_production_failed_action_refuses_before_any_raw_read_if_writer_remains(self):
        import ast,json,os,stat,tempfile
        from types import SimpleNamespace
        from unittest import mock
        import gnome_failed_capture as capture
        parsed=ast.parse((ROOT/'packaging/marlin/perf/roost-perf-phase').read_text())
        function=next(node for node in parsed.body if isinstance(node,ast.FunctionDef) and node.name=='gnome_trace')
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'capture';path.write_bytes(b'x'*256);path.chmod(0o600)
            uid=os.getuid();pid=os.getpid();start=capture.process_start(pid)
            source={'qualified':False,'pid':pid,'process_start':start,'capture_identity':capture.identity(path.stat())}
            raw=mock.Mock(side_effect=AssertionError('unclosed raw MUST NOT be read'))
            clock=iter([0,1,7])
            ns={'Path':lambda _:path,'pwd':SimpleNamespace(getpwnam=lambda _:SimpleNamespace(pw_uid=uid)),
                'subprocess':SimpleNamespace(run=lambda *a,**kw:SimpleNamespace(stdout=json.dumps(source)),CalledProcessError=RuntimeError),
                'json':json,'os':os,'stat':stat,'process_start':capture.process_start,'read_raw':raw,
                'capture_writers':lambda *a:[{'fd':3,'flags':1}],
                'time':SimpleNamespace(monotonic_ns=lambda:1,monotonic=lambda:next(clock),sleep=lambda _:None)}
            exec(compile(ast.Module(body=[function],type_ignores=[]),'<actual-failed-action>','exec'),ns)
            with self.assertRaisesRegex(RuntimeError,'writer did not close'):ns['gnome_trace']('failed-stop',0)
            raw.assert_not_called()


class FailureTransport(unittest.TestCase):
    def production_phase(self, raw):
        """Run the real phase body and validators on an original temporary file.

        The session/process and root receipt storage are controlled fixtures;
        raw FD identity, reading, schema, and chunk production use real helpers.
        """
        import ast,json,os,stat,tempfile,time
        from types import SimpleNamespace
        import gnome_failed_capture as capture
        saved,_=FailedCapture().fixture()
        uid=os.getuid()
        self.assertGreater(uid,0)
        saved['source']['uid']=uid;saved['failure']['trace_failure']['principal']['uid']=uid
        saved['source']['pid']=os.getpid();saved['failure']['trace_failure']['principal']['pid']=os.getpid()
        saved['source']['process_start']=capture.process_start(os.getpid())
        parsed=ast.parse((ROOT/'packaging/marlin/perf/roost-perf-phase').read_text())
        function=next(n for n in parsed.body if isinstance(n,ast.FunctionDef) and n.name=='gnome_trace')
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'capture';path.write_bytes(raw);path.chmod(0o600)
            saved['source']['capture_identity']=capture.identity(path.stat())
            payload=dict(saved['source'],qualified=False,failure=saved['failure'],
                         stop_requested_monotonic_ns=300,failed_stop_observed_monotonic_ns=400)
            receipts={}
            def store(name,value,owner):
                self.assertEqual(owner,0);capture.validate_receipt(value)
                receipts[str(name)]=copy.deepcopy(value)
            ns={name:getattr(capture,name) for name in ('validate_receipt','process_start','read_raw','identity','checked_file')}
            ns.update(Path=lambda name:path if name.endswith('.syscap') else Path(directory)/'receipt',
                      pwd=SimpleNamespace(getpwnam=lambda _:SimpleNamespace(pw_uid=uid)),
                      subprocess=SimpleNamespace(run=lambda *a,**kw:SimpleNamespace(stdout=json.dumps(payload)),CalledProcessError=RuntimeError),
                      json=json,os=os,stat=stat,time=time,base64=__import__('base64'),
                      capture_writers=lambda *a:[],write_marker=store,
                      read_marker=lambda name,owner:copy.deepcopy(receipts[str(name)]))
            exec(compile(ast.Module(body=[function],type_ignores=[]),'<actual-successful-failed-phase>','exec'),ns)
            metadata=ns['gnome_trace']('failed-stop',0)
            self.assertNotIn('guest_kernel',metadata)
            self.assertEqual(capture.validate_receipt(metadata),metadata)
            chunk=ns['gnome_trace']('failed-stop-read',0)
            self.assertEqual(capture.validate_chunk(chunk,0,metadata),raw)
            return metadata,chunk
    def test_actual_phase_closed_receipt_and_chunk_pass_shared_validators(self):
        self.production_phase(b'controlled-unqualified-trace'*100)
    def test_original_actual_exit9_survives_independent_raw_retention_or_refusal(self):
        import ast,base64,hashlib,json,math,subprocess,sys,tempfile,time
        from perf_trace_diagnostics import Context,encode,decode,MAX_BYTES
        parsed=ast.parse((ROOT/'scripts/roost-vm-perf').read_text())
        context=Context('stop');context.stage='stop-call'
        payload=encode(context.failure(RuntimeError('PRIVATE')),'stop').decode()
        child=subprocess.run([sys.executable,'-c','import sys;print(sys.argv[1]);sys.exit(9)',payload],capture_output=True)
        self.assertEqual(child.returncode,9)
        raw=b'controlled-unqualified-trace'*100
        capture_identity={'device':1,'inode':2,'uid':1000}
        for refusal in (None,'hash','writers'):
            refused=refusal is not None
            calls=[]
            saved,modules=FailedCapture().fixture()
            metadata=dict(saved['source'],qualified=False,failure=saved['failure'],
                          stop_requested_monotonic_ns=300,failed_stop_observed_monotonic_ns=400,
                          bytes=len(raw),sha256=hashlib.sha256(raw).hexdigest(),
                          capture_mtime_ns=1,capture_writers_at_failure=[],capture_writers_after_failure=[],
                          capture_writer_close_wait_ns=10,writer_closure_observed=True)
            if refusal is None:
                metadata,phase_chunk=self.production_phase(raw)
                capture_identity=metadata['capture_identity']
            if refusal=='hash':metadata['sha256']='invalid'
            if refusal=='writers':metadata['writer_closure_observed']=False;metadata['capture_writers_after_failure']=[{'fd':3,'flags':1}]
            class Agent:
                action=None
                def command(self,method,**kwargs):
                    if method=='guest-exec':
                        self.action=kwargs['arg'][0];calls.append(kwargs['arg']);return {'pid':1}
                    if self.action=='gnome-trace-stop':
                        return {'exited':True,'exitcode':child.returncode,'out-data':base64.b64encode(child.stdout).decode()}
                    value=metadata if self.action=='gnome-trace-failed-stop' else (phase_chunk if refusal is None else {
                        'index':0,'data':base64.b64encode(raw).decode(),'capture_identity':capture_identity})
                    return {'exited':True,'exitcode':0,'out-data':base64.b64encode(json.dumps({'boottime_s':1,'gnome_trace':value}).encode()).decode()}
            ns={'base64':base64,'hashlib':hashlib,'json':json,'math':math,'time':time,'sys':sys,
                'decode_trace_failure':decode,'TRACE_FAILURE_BYTES':MAX_BYTES}
            from gnome_failed_capture import validate_receipt,validate_chunk
            ns.update(validate_failed_capture=validate_receipt,validate_failed_chunk=validate_chunk)
            names={'guest_probe','retain_trace_failure','retain_failed_gnome_capture'}
            exec(compile(ast.Module(body=[n for n in parsed.body if isinstance(n,ast.FunctionDef) and n.name in names],type_ignores=[]),'<actual-failed-capture-host>','exec'),ns)
            with tempfile.TemporaryDirectory() as directory:
                out=Path(directory)
                with self.assertRaises(RuntimeError):ns['guest_probe'](Agent(),'gnome-trace-stop',out=out)
                failure=json.loads((out/'gnome-trace-stop-failure.json').read_text())
                self.assertEqual(failure['guest_exitcode'],9);self.assertTrue(failure['diagnostic_accepted'])
                report=json.loads((out/'gnome-overview-failed-acquisition.json').read_text())
                self.assertFalse(report['qualified']);self.assertEqual(report['raw_retained'],not refused)
                if not refused:
                    self.assertEqual((out/'gnome-overview-failed.syscap').read_bytes(),raw)
                    self.assertTrue(report['writer_closure_observed'])
                self.assertNotIn('PRIVATE',''.join(p.read_text(errors='ignore') for p in out.iterdir()))
            self.assertEqual(calls[0],['gnome-trace-stop']);self.assertEqual(calls[1],['gnome-trace-failed-stop'])
            if not refused:self.assertEqual(calls[2],['gnome-trace-failed-stop-read','--index','0'])


def diagnostic_package_policy(recipe, container, receiver, host):
    import re
    def scalar(name):
        rows=re.findall(r'^'+name+r'=([^\n]+)$',recipe,re.M)
        if len(rows)!=1:raise ValueError('missing or duplicate '+name)
        return rows[0]
    rel=scalar('pkgrel');version=scalar('pkgver')
    # Arch PKGBUILD(5): positive integer, optional positive integer subrelease.
    if not re.fullmatch(r'[1-9][0-9]*(?:\.[1-9][0-9]*)?',rel):
        raise ValueError('invalid Arch pkgrel')
    package='mutter '+version+'-'+rel
    archive='mutter-'+version+'-'+rel+'-x86_64.pkg.tar.zst'
    archives=re.findall(r'mutter-[0-9.]+-[0-9.]+-x86_64\.pkg\.tar\.zst',container)
    if len(archives)!=3 or any(item!=archive for item in archives):
        raise ValueError('baseline archive pin mismatch')
    pins=re.findall(r"mutter [0-9.]+-[0-9.]+",container)
    if pins!=[package]:raise ValueError('baseline installed pin mismatch')
    if re.findall(r'mutter [0-9.]+-[0-9.]+',receiver)!=[package]:
        raise ValueError('mapped receiver package pin mismatch')
    if re.findall(r'mutter [0-9.]+-[0-9.]+',host).count(package)!=3:
        raise ValueError('host source-evidence and boundary pin mismatch')
    if 'if package == '+repr(package)+':' not in host and 'if package == "'+package+'":' not in host:
        gate=re.search(r'if package in \([^)]*"'+re.escape(package)+r'"[^)]*\):\s*\n\s*stop_boundary =',host)
        if gate is None:
            raise ValueError('host boundary package pin mismatch')
    return package

class PackagePolicy(unittest.TestCase):
    def actual(self):
        return [(ROOT/n).read_text() for n in ['packaging/marlin/perf/mutter-profiler/PKGBUILD',
                'packaging/marlin/perf/Containerfile.baseline','packaging/marlin/perf/roost-gnome-profiler','scripts/roost-vm-perf']]
    def test_actual_recipe_and_all_acquisition_pins_agree(self):
        self.assertEqual(diagnostic_package_policy(*self.actual()),'mutter 51.0-1.7')
    def test_original_invalid_three_component_release_rejected(self):
        a=self.actual();a[0]=a[0].replace('pkgrel=1.7','pkgrel=1.2.1')
        with self.assertRaises(ValueError):diagnostic_package_policy(*a)
    def test_zero_negative_alpha_missing_duplicate_releases_rejected(self):
        for bad in ['0','1.0','-1','one','1.7.1','1.7\npkgrel=1.7']:
            a=self.actual();a[0]=a[0].replace('pkgrel=1.7','pkgrel='+bad)
            with self.subTest(bad=bad),self.assertRaises(ValueError):diagnostic_package_policy(*a)
        a=self.actual();a[0]=a[0].replace('pkgrel=1.7\n','')
        with self.assertRaises(ValueError):diagnostic_package_policy(*a)
    def test_stale_builder_install_receiver_and_host_pins_rejected(self):
        for lane in [1,2,3]:
            a=self.actual();a[lane]=a[lane].replace('51.0-1.7','51.0-1.2',1)
            with self.subTest(lane=lane),self.assertRaises(ValueError):diagnostic_package_policy(*a)
    def test_missing_archive_or_boundary_gate_rejected(self):
        for lane,needle in [(1,'mutter-51.0-1.7-x86_64.pkg.tar.zst'),(3,'if package in ("mutter 51.0-1.6", "mutter 51.0-1.7"):')]:
            a=self.actual();a[lane]=a[lane].replace(needle,'removed',1)
            with self.subTest(lane=lane),self.assertRaises(ValueError):diagnostic_package_policy(*a)

if __name__=='__main__':unittest.main()
