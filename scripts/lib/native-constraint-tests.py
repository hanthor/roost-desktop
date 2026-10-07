#!/usr/bin/env python3
"""Fixed native principal control and identity rejection; no build/codegen."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('native_constraint_lifecycle', str(ROOT/'packaging/marlin/vm-lane/roost-vm-lifecycle'))
spec = importlib.util.spec_from_loader(loader.name, loader)
fixture = importlib.util.module_from_spec(spec)
with patch('pwd.getpwnam', return_value=SimpleNamespace(pw_uid=1000,pw_name='roost-test')):
    loader.exec_module(fixture)


class Principal(unittest.TestCase):
    def identity(self):
        return {'pid':100,'uid':1000,'start_ticks':1234,'executable':'/usr/libexec/roost-vm-constraints',
                'exe_inode':55,'exe_device':2049,'exe_sha256':'actual'}
    def snapshot(self):
        return {'pid':100,'uid':1000,'surface_id':8,'pointer_id':9,'relative_id':10}
    def invoke(self, tmp, identity=None, response=None, action='status'):
        owner = Path(tmp)/'owner.json'
        owner.write_text(json.dumps({'identity':self.identity(),'surface_id':8,'pointer_id':9,'relative_id':10}))
        with patch.object(fixture,'CONSTRAINT_OWNER',owner), patch.object(fixture,'constraint_process',return_value=identity or self.identity()), patch.object(fixture,'call',return_value=json.dumps(response or self.snapshot())) as call:
            value = fixture.constraint_client(action)
            self.assertEqual(call.call_args.args, ('/usr/libexec/roost-vm-constraints','--command',action))
            self.assertEqual(call.call_args.kwargs, {'user':True})
            return value
    def test_same_principal_and_protocol_ids_are_retained(self):
        with tempfile.TemporaryDirectory() as tmp:
            value=self.invoke(tmp)
            self.assertEqual(value['principal']['identity'],self.identity())
            self.assertEqual(value['client'],self.snapshot())
    def test_pid_reuse_exec_inode_or_binary_rotation_is_rejected(self):
        for field,value in (('pid',101),('start_ticks',1235),('exe_inode',56),('exe_sha256','changed')):
            identity=self.identity();identity[field]=value
            with self.subTest(field=field),tempfile.TemporaryDirectory() as tmp:
                with self.assertRaisesRegex(RuntimeError,'process changed'):
                    self.invoke(tmp,identity=identity)
    def test_response_from_wrong_uid_or_pid_is_rejected(self):
        for field,value in (('uid',0),('pid',101)):
            response=self.snapshot();response[field]=value
            with self.subTest(field=field),tempfile.TemporaryDirectory() as tmp:
                with self.assertRaisesRegex(RuntimeError,'differs from original'):
                    self.invoke(tmp,response=response)
    def test_replaced_surface_pointer_or_relative_object_is_rejected(self):
        for field in ('surface_id','pointer_id','relative_id'):
            response=self.snapshot();response[field]+=1
            with self.subTest(field=field),tempfile.TemporaryDirectory() as tmp:
                with self.assertRaisesRegex(RuntimeError,'protocol objects changed'):
                    self.invoke(tmp,response=response)
    def test_fixed_action_rejects_arbitrary_command_and_path(self):
        for command in ('/bin/sh','lock;id','status extra','../../escape'):
            with self.subTest(command=command),self.assertRaisesRegex(ValueError,'fixed constraint action'):
                fixture.constraint_client(command)


if __name__ == '__main__': unittest.main()
