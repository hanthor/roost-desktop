#!/usr/bin/env python3
"""Production unsupported policy/owned-file negatives; no GTK/GPU qualifier."""
import copy
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[2]
loader=importlib.machinery.SourceFileLoader('unsupported',str(ROOT/'scripts/lib/night-light-unsupported-proof.py'))
spec=importlib.util.spec_from_loader(loader.name,loader)
proof=importlib.util.module_from_spec(spec);loader.exec_module(proof)


def state():
    return {'locked':False,'night_light':{'supported':False,'service_supported':False,'owner_epoch':0,
            'generation':1,'temperature':6500,'service_rgb_scales':[1.,1.,1.]}}


class Unsupported(unittest.TestCase):
    def test_actual_snapshot_policy_refuses_locked_supported_warm_missing_and_boolean_types(self):
        proof.unsupported_state(state())
        for key,value in (('supported',True),('service_supported',True),('owner_epoch',True),('owner_epoch',1),
                          ('generation',True),('generation',0),('temperature',4000),('service_rgb_scales',[True,1.,1.]),
                          ('service_rgb_scales',[1.,.8,.5])):
            bad=state();bad['night_light'][key]=value
            with self.subTest(key=key,value=value),self.assertRaises(ValueError):proof.unsupported_state(bad)
        for bad in ({},dict(state(),locked=True),dict(state(),locked=0)):
            with self.assertRaises(ValueError):proof.unsupported_state(bad)

    def test_actual_tree_policy_refuses_visible_tile_ambiguous_types_and_missing_tree(self):
        self.assertEqual(proof.absent_visible_tile([{'name':'Night Light','showing':False}]),1)
        for nodes in ([],[{'name':'Night Light','showing':True}],[{'name':'Panel','showing':0}],
                      [{'name':'Panel'}],[{'name':'Panel','showing':False}]*4097):
            with self.assertRaises(ValueError):proof.absent_visible_tile(nodes)

    def test_real_owned_reader_refuses_symlink_fifo_oversize(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'state';path.write_text(json.dumps(state()))
            value,receipt=proof.read_owned(path,1024);self.assertEqual(value,state())
            self.assertEqual(receipt['bytes'],path.stat().st_size)
            with self.assertRaises(ValueError):proof.read_owned(path,2)
            link=Path(directory)/'link';link.symlink_to(path)
            with self.assertRaises(OSError):proof.read_owned(link,1024)
            fifo=Path(directory)/'fifo';os.mkfifo(fifo)
            with self.assertRaises(ValueError):proof.read_owned(fifo,1024)

    def test_original_kernel_start_reader_uses_real_owned_process(self):
        first=proof.original_start(os.getpid());self.assertEqual(first,proof.original_start(os.getpid()))
        for pid in (True,0,-1):
            with self.assertRaises(ValueError):proof.original_start(pid)

    def test_actual_prove_rejects_service_principal_capability_and_key_mismatch_without_success_receipt(self):
        uid=os.getuid()
        class Variant:
            def __init__(self,signature,values):self.values=values
        for scenario in ('valid','wrong-pid','wrong-uid','wrong-start','owner-replaced','supported','key-changed','color-owner'):
            calls=[]
            def bus_call(*args):
                method=args[3];calls.append(method)
                result={'GetNameOwner':':1.71','GetConnectionUnixProcessID':71,'GetConnectionUnixUser':uid,
                        'NameHasOwner':False,'Get':False}[method]
                if scenario=='wrong-pid' and method=='GetConnectionUnixProcessID':result=72
                if scenario=='wrong-uid' and method=='GetConnectionUnixUser':result=uid+1
                if scenario=='owner-replaced' and method=='GetNameOwner' and calls.count(method)>2:result=':1.72'
                if scenario=='supported' and method=='Get':result=True
                if scenario=='color-owner' and method=='NameHasOwner':result=True
                return SimpleNamespace(unpack=lambda:(result,))
            bus=SimpleNamespace(call_sync=bus_call)
            gio=SimpleNamespace(bus_get_sync=lambda *args:bus,BusType=SimpleNamespace(SESSION=1),
                                DBusCallFlags=SimpleNamespace(NONE=0),
                                Settings=SimpleNamespace(new=lambda name:SimpleNamespace(get_boolean=lambda key:scenario=='key-changed')))
            glib=SimpleNamespace(Variant=Variant,VariantType=SimpleNamespace(new=lambda value:value))
            with tempfile.TemporaryDirectory() as directory:
                root=Path(directory);s=root/'state';s.write_text(json.dumps(state()))
                tree=root/'tree';tree.write_text(json.dumps([{'name':'Panel','showing':True}]))
                args=SimpleNamespace(pid=71,start=222,exe='/known/roost-compositor',state=s,tree=tree,out=root/'receipt')
                with patch.dict(sys.modules,{'gi':SimpleNamespace(),'gi.repository':SimpleNamespace(Gio=gio,GLib=glib)}), \
                     patch.object(proof.os,'getuid',return_value=uid), \
                     patch.object(proof,'original_start',return_value=223 if scenario=='wrong-start' else 222), \
                     patch.object(proof.Path,'resolve',return_value=Path('/known/roost-compositor')):
                    if scenario=='valid':
                        proof.prove(args)
                        self.assertEqual(json.loads(args.out.read_text())['scope'],'unsupported-negative-only')
                    else:
                        with self.assertRaises(ValueError):proof.prove(args)
                        self.assertFalse(args.out.exists())


if __name__=='__main__':unittest.main()
