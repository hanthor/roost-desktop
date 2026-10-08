#!/usr/bin/python3
"""Actual bounded handoff file policy tests; no compositor or provider claim."""
import ast
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest

SOURCE = Path(__file__).with_name('roost-night-light-fault-controller.py')
module = ast.parse(SOURCE.read_text())
functions = ast.Module(body=[node for node in module.body if isinstance(node,ast.FunctionDef) and node.name in ('read_request','publish','compare_admitted')],type_ignores=[])
namespace = {'os':os,'stat':stat,'json':json,'Path':Path}
exec(compile(functions,str(SOURCE),'exec'),namespace)
proof=SOURCE.with_name('roost-night-light-proof.py')
production_capture=ast.Module(body=[node for node in ast.parse(proof.read_text()).body
                                   if isinstance(node,ast.FunctionDef) and node.name=='capture_baseline_receipt'],type_ignores=[])
exec(compile(production_capture,str(proof),'exec'),namespace)


class Handoff(unittest.TestCase):
    def setUp(self):
        self.directory=tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.out=Path(self.directory.name)
        namespace['OUT']=self.out
        self.request={'pid':1234,'start':999,'exe_sha256':'a'*64,
                      'baseline_receipt':{'sha256':'b'*64,'dev':1,'inode':2,'size':100,'mtime_ns':3,'ctime_ns':4,'observed_wall_ns':10_000_000_000},
                      'baseline_transform':{'owner_epoch':3,'generation':4,'temperature':3700,'service_rgb_scales':[1.,.8,.5],
                                            'outputs':[{'last_submitted_transform':[3,4,5,[1.,.8,.5]]}]}}
    def write(self,value):
        (self.out/'fault-request.json').write_text(json.dumps(value))
    def test_original_bounded_ordinary_uid_request(self):
        # Uses the real invoking UID; CI runs as UID1000, like the fixture.
        self.assertEqual(os.getuid(),1000)
        self.write(self.request)
        self.assertEqual(namespace['read_request'](),self.request)
    def test_symlink_even_to_original_contents_rejected(self):
        target=self.out/'other.json'; target.write_text(json.dumps(self.request))
        (self.out/'fault-request.json').symlink_to(target)
        with self.assertRaises(OSError): namespace['read_request']()
    def test_nonregular_fifo_cannot_block_or_be_admitted(self):
        os.mkfifo(self.out/'fault-request.json')
        with self.assertRaises(RuntimeError): namespace['read_request']()
    def test_oversized_real_regular_request_rejected(self):
        (self.out/'fault-request.json').write_bytes(b' '*8193)
        with self.assertRaises(RuntimeError): namespace['read_request']()
    def test_unknown_process_or_receipt_fields_rejected(self):
        for value in [dict(self.request,command='/unapproved/helper'),
                      dict(self.request,baseline_receipt=dict(self.request['baseline_receipt'],extra=True)),
                      dict(self.request,pid=1),dict(self.request,start='999'),dict(self.request,start=True)]:
            self.write(value)
            with self.assertRaises(RuntimeError): namespace['read_request']()
    def test_slow_actual_capture_and_fd_read_cannot_renew_admission_budget(self):
        later_fd=dict(self.request['baseline_receipt'],observed_wall_ns=16_000_000_000)
        baseline=namespace['capture_baseline_receipt'](later_fd,10_000_000_000)
        self.assertEqual(later_fd['observed_wall_ns'],16_000_000_000)
        self.assertEqual(baseline['observed_wall_ns'],10_000_000_000)
        request=dict(self.request,baseline_receipt=baseline)
        with self.assertRaises(RuntimeError):
            namespace['compare_admitted'](request,{'transform':request['baseline_transform'],
                                                  'admitted_wall_ns':16_000_000_000})
        with self.assertRaises(RuntimeError):
            namespace['capture_baseline_receipt'](later_fd,16_000_000_001)
    def test_fresh_same_transform_accepts_new_snapshot_inode_and_hash(self):
        admitted={'transform':self.request['baseline_transform'],'admitted_wall_ns':12_000_000_000,
                  'sha256':'c'*64,'dev':1,'inode':99,'size':130}
        compared=namespace['compare_admitted'](self.request,admitted)
        self.assertEqual(compared['capture_to_admission_ns'],2_000_000_000)
        self.assertFalse(compared['same_snapshot_required'])
        self.assertNotEqual(compared['baseline_receipt']['inode'],compared['admitted_receipt']['inode'])
    def test_changed_owner_generation_rgb_or_output_submission_rejected(self):
        for field,value in [('owner_epoch',99),('generation',99),('temperature',6500),
                            ('service_rgb_scales',[1.,1.,1.]),('outputs',[])]:
            transform=dict(self.request['baseline_transform']); transform[field]=value
            with self.assertRaises(RuntimeError):
                namespace['compare_admitted'](self.request,{'transform':transform,'admitted_wall_ns':12_000_000_000})
    def test_expired_and_backward_capture_intervals_rejected(self):
        for wall in [9_999_999_999,15_000_000_001]:
            with self.assertRaises(RuntimeError):
                namespace['compare_admitted'](self.request,{'transform':self.request['baseline_transform'],'admitted_wall_ns':wall})
    def test_result_does_not_overwrite_or_follow_existing_path(self):
        target=self.out/'original'; target.write_text('original')
        (self.out/'result.json').symlink_to(target)
        with self.assertRaises(FileExistsError): namespace['publish']('result.json',{'ok':True})
        self.assertEqual(target.read_text(),'original')
    def test_result_output_bound_is_checked_before_creation(self):
        with self.assertRaises(RuntimeError): namespace['publish']('result.json',{'value':'x'*16385})
        self.assertFalse((self.out/'result.json').exists())


if __name__=='__main__': unittest.main()
