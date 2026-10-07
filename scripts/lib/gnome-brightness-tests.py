#!/usr/bin/env python3
"""Actual wire schema parser negative cases; no fake runtime provider."""
import ast
from pathlib import Path
import re
import os
import stat
import json
import tempfile
import time
from types import SimpleNamespace
from unittest import mock
import unittest
import xml.etree.ElementTree as ET

ROOT=Path(__file__).resolve().parents[2]
parsed=ast.parse((ROOT/'scripts/lib/gnome-brightness-proof.py').read_text())
namespace={'ET':ET,'NAME':'org.gnome.Shell.Brightness','os':os,'stat':stat,'json':json,'Path':Path,'time':time,'PATH':'/org/gnome/Shell/Brightness','GLib':SimpleNamespace(Variant=lambda signature,args:args)}
function=next(node for node in parsed.body if isinstance(node,ast.FunctionDef) and node.name=='schema')
exec(compile(ast.Module(body=[function],type_ignores=[]),'<actual-schema-parser>','exec'),namespace)
schema=namespace['schema']
for name in ['scalar','retain','process','principal','available','signal_receipt','drain']:
    node=next(node for node in parsed.body if isinstance(node,ast.FunctionDef) and node.name==name)
    exec(compile(ast.Module(body=[node],type_ignores=[]),'<actual-bounded-proof-io>','exec'),namespace)
scalar=namespace['scalar'];retain=namespace['retain']
XML=re.search(r'const XML: &str = r#"(.*?)"#;', (ROOT/'crates/shell-gtk/src/brightness_service.rs').read_text(),re.S).group(1)

class Contract(unittest.TestCase):
    def test_current_service_schema_matches_gnome51_wire(self):schema(XML)
    def test_changed_property_type_access_or_name_rejected(self):
        for before,after in [('type="b" access="read"','type="i" access="read"'),('access="read"','access="readwrite"'),('HasBrightnessControl','Brightness')]:
            with self.subTest(after=after),self.assertRaises(RuntimeError):schema(XML.replace(before,after))
    def test_wrong_method_argument_direction_or_missing_method_rejected(self):
        for before,after in [('type="d"','type="i"'),('direction="in"','direction="out"'),('SetDimming','OtherMethod')]:
            with self.subTest(after=after),self.assertRaises(RuntimeError):schema(XML.replace(before,after))
    def test_signal_payload_duplicate_method_or_extra_interface_rejected(self):
        for addition in ['<method name="SetDimming"><arg type="b" direction="in"/></method>',
                         '<signal name="OtherSignal"/>']:
            with self.subTest(addition=addition),self.assertRaises(RuntimeError):schema(XML.replace('</interface>',addition+'</interface>'))
        with self.assertRaises(RuntimeError):schema(XML.replace('<signal name="BrightnessChanged"/>','<signal name="BrightnessChanged"><arg type="i"/></signal>'))
        with self.assertRaises(RuntimeError):schema('<node>'+XML+XML+'</node>')
    def test_missing_interface_and_oversized_introspection_rejected(self):
        with self.assertRaises(RuntimeError):schema('<node/>')
        with self.assertRaises(RuntimeError):schema(' '*65537)

class ActualBoundedIO(unittest.TestCase):
    def test_scalar_bounds_numeric_range_and_nonregular(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'scalar'
            path.write_text('4294967295');self.assertEqual(scalar(path),4294967295)
            for value in ['1'*65,'-1','4294967296','NaN','1 2']:
                path.write_text(value)
                with self.subTest(value=value),self.assertRaises((RuntimeError,ValueError)):scalar(path)
            link=Path(directory)/'link';link.symlink_to(path)
            with self.assertRaises(OSError):scalar(link)
            fifo=Path(directory)/'fifo';os.mkfifo(fifo)
            with self.assertRaises(RuntimeError):scalar(fifo)
    def test_scalar_permission_failure_propagates(self):
        with mock.patch.object(os,'open',side_effect=PermissionError('actual open refusal')):
            with self.assertRaises(PermissionError):scalar('/not-authorized')
    def test_replaced_scalar_identity_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'scalar';path.write_text('505')
            replacement=Path(directory)/'other';replacement.write_text('700')
            actual_read=os.read
            def replace_after_read(fd,count):
                value=actual_read(fd,count);os.replace(replacement,path);return value
            with mock.patch.object(os,'read',side_effect=replace_after_read),self.assertRaises(RuntimeError):scalar(path)
    def test_receipt_exclusive_bounded_and_does_not_replace_existing(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'receipt'
            retain(path,{'pass':False});self.assertEqual(json.loads(path.read_text()),{'pass':False})
            with self.assertRaises(FileExistsError):retain(path,{'pass':True})
            self.assertFalse(json.loads(path.read_text())['pass'])
            other=Path(directory)/'too-large'
            with self.assertRaises(RuntimeError):retain(other,{'data':'x'*1048576})
            self.assertFalse(other.exists())
    def test_receipt_named_replacement_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'receipt';replacement=Path(directory)/'replacement';replacement.write_text('{}')
            actual_stat=os.stat
            def replaced(name,**kwargs):
                os.replace(replacement,path);return actual_stat(name,**kwargs)
            with mock.patch.object(os,'stat',side_effect=replaced),self.assertRaises(RuntimeError):retain(path,{'pass':False})

class OriginalOwner(unittest.TestCase):
    def test_capability_call_targets_original_unique_owner(self):
        calls=[]
        def call(bus,dest,*args):
            calls.append(dest);return SimpleNamespace(unpack=lambda:(False,))
        with mock.patch.dict(namespace,{'call':call}):
            self.assertFalse(namespace['available'](object(),':1.42'))
        self.assertEqual(calls,[':1.42'])
    def test_invalid_process_wrong_executable_and_root_refused(self):
        for pid in [0,-1]:
            with self.assertRaises(RuntimeError):namespace['process'](pid,'/usr/bin/roost-shell-gtk')
        with self.assertRaises(RuntimeError):namespace['process'](os.getpid(),'/definitely-not-the-original-shell')
        with mock.patch.object(Path,'stat',return_value=SimpleNamespace(st_uid=0)), mock.patch.object(os,'readlink',return_value='/usr/bin/roost-shell-gtk'):
            with self.assertRaises(RuntimeError):namespace['process'](os.getpid(),'/usr/bin/roost-shell-gtk')
    def test_replaced_bus_owner_refused_before_process_observation(self):
        def call(bus,dest,path,interface,method,args):
            return SimpleNamespace(unpack=lambda:(':1.42' if args[0]==namespace['NAME'] else ':1.43',))
        with mock.patch.dict(namespace,{'call':call,'GLib':SimpleNamespace(Variant=lambda signature,args:args)}):
            with self.assertRaises(RuntimeError):namespace['principal'](object(),'/usr/bin/roost-shell-gtk')
    def test_native_original_start_or_compositor_identity_mismatch_refused(self):
        shell=dict(pid=10,uid=1000,start_ticks=20,executable='/usr/bin/roost-shell-gtk',parent_pid=11)
        compositor=dict(pid=11,uid=1000,start_ticks=21,executable='/usr/bin/roost-compositor',parent_pid=1)
        def call(bus,dest,path,interface,method,args):
            value=10 if method=='GetConnectionUnixProcessID' else 1000 if method=='GetConnectionUnixUser' else ':1.42'
            return SimpleNamespace(unpack=lambda:(value,))
        def process(pid,exe):return shell if pid==10 else compositor
        with mock.patch.dict(namespace,{'call':call,'process':process,'GLib':SimpleNamespace(Variant=lambda signature,args:args)}), mock.patch.object(os,'readlink',return_value='/usr/bin/roost-compositor'):
            observed=namespace['principal'](object(),shell['executable'])
            self.assertEqual(observed['compositor'],compositor)
            for expected in [{'start_ticks':19},{'pid':9},{'compositor':dict(compositor,start_ticks=22)}]:
                with self.subTest(expected=expected),self.assertRaises(RuntimeError):namespace['principal'](object(),shell['executable'],expected)

class BoundedSignalDispatch(unittest.TestCase):
    def test_original_signal_overflow_retained_without_callback_exception(self):
        signals=[];overflow=[False]
        for _ in range(513):namespace['signal_receipt'](signals,overflow)
        self.assertEqual(len(signals),512);self.assertTrue(overflow[0])
    def test_continuous_pending_queue_refused_at_fixed_iteration_bound(self):
        iterations=[];context=SimpleNamespace(pending=lambda:True,iteration=lambda block:iterations.append(block))
        with self.assertRaises(RuntimeError):namespace['drain'](context,time.monotonic()+5,[100])
        self.assertEqual(len(iterations),100);self.assertEqual(set(iterations),{False})
    def test_deadline_refuses_dispatch_and_finite_queue_preserves_remaining_budget(self):
        iterations=[];context=SimpleNamespace(pending=lambda:len(iterations)<2,iteration=lambda block:iterations.append(block))
        with self.assertRaises(RuntimeError):namespace['drain'](context,time.monotonic()-1,[100])
        self.assertEqual(iterations,[])
        budget=[100];namespace['drain'](context,time.monotonic()+5,budget)
        self.assertEqual((len(iterations),budget[0]),(2,98))

if __name__=='__main__':unittest.main()
