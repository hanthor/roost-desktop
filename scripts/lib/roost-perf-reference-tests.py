#!/usr/bin/env python3
"""Production receipt/profile/clock policy tests; no stock runtime claims."""
import copy
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT/'scripts/lib'))
import gnome_reference as policy


def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


reference = load('stock_reference', ROOT/'packaging/marlin/perf/roost-gnome-reference')


def image(profile='stock'):
    return dict(schema=1, profile=profile, mutter_package='mutter 51.0-1.6' if profile=='diagnostic' else 'mutter 51.0-1',
                shell_package='gnome-shell 1:51.0-1', packages_sha256='a'*64, critical_sha256='b'*64)


def receipt():
    value = dict(image=image(), principal=dict(uid=1000,pid=71,start=222,owner=':1.71'),
                 shell_version='GNOME Shell 51.0', shell_sha256='c'*64, modules={})
    for name in ('core','cogl','clutter'):
        value['modules'][name] = dict(path=f'/usr/lib/libmutter-{name}-51.so',device=20,inode=30,uid=0,bytes=100,
                                       sha256='d'*64,package='mutter 51.0-1')
    return value


class ReferencePolicy(unittest.TestCase):
    def test_explicit_profiles_never_select_trace_fallback(self):
        self.assertTrue(policy.trace_required('gnome','diagnostic'))
        self.assertFalse(policy.trace_required('gnome','stock'))
        self.assertFalse(policy.trace_required('roost','diagnostic'))
        for desktop,profile in (('gnome','automatic'),('shell','stock'),('gnome',None)):
            with self.assertRaises(ValueError):policy.trace_required(desktop,profile)
        with self.assertRaises(ValueError):policy.validate_image(image(), 'diagnostic')
        bad=image('diagnostic');bad['mutter_package']='mutter 51.0-1'
        with self.assertRaises(ValueError):policy.validate_image(bad,'diagnostic')

    def test_missing_unknown_wrong_types_and_private_payload_rejected(self):
        original=receipt()
        policy.validate_reference(original,'stock')
        bads=[]
        for key in original:
            bad=copy.deepcopy(original);del bad[key];bads.append(bad)
        bad=copy.deepcopy(original);bad['private_body']='PRIVATE';bads.append(bad)
        for key in ('uid','pid','start'):
            for value in (True,0,-1,'1000'):
                bad=copy.deepcopy(original);bad['principal'][key]=value;bads.append(bad)
        for key,value in (('uid',True),('bytes',True),('package','mutter 50.5-1'),('sha256','bad'),('path','/tmp/private')):
            bad=copy.deepcopy(original);bad['modules']['core'][key]=value;bads.append(bad)
        bad=copy.deepcopy(original);del bad['modules']['clutter'];bads.append(bad)
        for bad in bads:
            with self.subTest(bad=bad),self.assertRaises(ValueError):policy.validate_reference(bad,'stock')

    def test_original_owner_start_module_image_change_refused(self):
        original=receipt();policy.same_reference(original,copy.deepcopy(original),'stock')
        for field,value in (('owner',':1.72'),('pid',72),('start',223)):
            bad=copy.deepcopy(original);bad['principal'][field]=value
            with self.assertRaises(ValueError):policy.same_reference(original,bad,'stock')
        bad=copy.deepcopy(original);bad['modules']['core']['inode']=31
        with self.assertRaises(ValueError):policy.same_reference(original,bad,'stock')
        bad=copy.deepcopy(original);bad['image']['critical_sha256']='e'*64
        with self.assertRaises(ValueError):policy.same_reference(original,bad,'stock')

    def test_idle_pss_counts_only_wholly_guest_idle_collections(self):
        samples=[dict(boottime_s=11+i,cpu_interval_start_boottime_s=10.75+i,pss_kib=100+i,rss_kib=200+i) for i in range(25)]
        samples += [dict(boottime_s=10.1,cpu_interval_start_boottime_s=9.9,pss_kib=999999),dict(boottime_s=40,cpu_interval_start_boottime_s=39.8,pss_kib=999999)]
        self.assertEqual(policy.idle_pss(samples,10,36),list(range(100,125)))
        self.assertEqual(policy.idle_memory(samples,10,36,'rss_kib'),list(range(200,225)))
        with self.assertRaises(ValueError):policy.idle_memory(samples,10,36,'private_body')
        for invalid in ([],samples[:19]):
            with self.assertRaises(ValueError):policy.idle_pss(invalid,10,36)
        for field,value in (('pss_kib',True),('pss_kib',None),('boottime_s',float('nan')),('cpu_interval_start_boottime_s',float('nan'))):
            bad=copy.deepcopy(samples);bad[0][field]=value
            with self.assertRaises(ValueError):policy.idle_pss(bad,10,36)

    def test_actual_protected_reader_refuses_symlink_oversize_unowned_and_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'receipt';path.write_bytes(b'valid');path.chmod(0o444)
            # Root ownership is mocked only to exercise the reader from any UID;
            # actual production retains its strict original-FD UID0 gate.
            real_fstat,real_stat=os.fstat,os.stat
            from types import SimpleNamespace
            def root_info(data):
                fields=('st_dev','st_ino','st_mode','st_uid','st_size','st_mtime_ns','st_ctime_ns')
                result={key:getattr(data,key) for key in fields};result['st_uid']=0
                return SimpleNamespace(**result)
            def info(fd):return root_info(real_fstat(fd))
            def named(target,**kwargs):return root_info(real_stat(target,**kwargs))
            with patch.object(reference.os,'fstat',side_effect=info),patch.object(reference.os,'stat',side_effect=named):
                self.assertEqual(reference.protected(str(path),10),b'valid')
                with self.assertRaises(ValueError):reference.protected(str(path),2)
                def replaced(target,**kwargs):
                    path.unlink();path.write_bytes(b'other');path.chmod(0o444)
                    return named(target,**kwargs)
                with patch.object(reference.os,'stat',side_effect=replaced):
                    with self.assertRaises(ValueError):reference.protected(str(path),10)
            # Ordinary UID1000 actual file is rejected without policy mocks.
            if os.getuid()!=0:
                with self.assertRaises(ValueError):reference.protected(str(path),10)
            link=Path(directory)/'link';link.symlink_to(path)
            with self.assertRaises(OSError):reference.protected(str(link),10)
            path.chmod(0o666)
            with patch.object(reference.os,'fstat',side_effect=info):
                with self.assertRaises(ValueError):reference.protected(str(path),10)

    def test_actual_host_receipt_reader_rejects_unknown_oversize_symlink_and_profile_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'image.json';path.write_text(json.dumps(image()));path.chmod(0o644)
            self.assertEqual(policy.read_image(path,'stock'),image())
            with self.assertRaises(ValueError):policy.read_image(path,'diagnostic')
            bad=image();bad['private_body']='PRIVATE';path.write_text(json.dumps(bad))
            with self.assertRaises(ValueError):policy.read_image(path,'stock')
            path.write_bytes(b' '*4097)
            with self.assertRaises(ValueError):policy.read_image(path,'stock')
            path.write_text(json.dumps(image()));link=Path(directory)/'link';link.symlink_to(path)
            with self.assertRaises(OSError):policy.read_image(link,'stock')
            path.chmod(0o666)
            with self.assertRaises(ValueError):policy.read_image(path,'stock')

    def test_actual_stock_main_rejects_owner_replacement_and_module_hash_mismatch(self):
        from types import SimpleNamespace
        import hashlib
        import io
        from contextlib import redirect_stdout
        original=receipt();shell=b'known-shell';original['shell_sha256']=hashlib.sha256(shell).hexdigest()
        critical=''.join(module['sha256']+'  '+module['path']+'\n' for module in original['modules'].values())
        critical+=original['shell_sha256']+'  /usr/bin/gnome-shell\n'
        def protected(path,limit):
            if path.endswith('critical.sha256'):return critical.encode()
            if path=='/usr/bin/gnome-shell':return shell
            raise AssertionError(path)
        class Variant:
            def __init__(self,signature,values):self.values=values
        for scenario in ('valid','changed-owner','changed-module'):
            calls=[]
            def bus_call(*args):
                method=args[3];calls.append(method)
                values={'GetNameOwner':':1.71','GetConnectionUnixProcessID':71,'GetConnectionUnixUser':1000}
                if scenario=='changed-owner' and calls.count('GetNameOwner')>2:
                    values['GetNameOwner']=':1.72'
                return SimpleNamespace(unpack=lambda:(values[method],))
            gio=SimpleNamespace(bus_get_sync=lambda *args:SimpleNamespace(call_sync=bus_call),
                                BusType=SimpleNamespace(SESSION=1),DBusCallFlags=SimpleNamespace(NONE=0))
            glib=SimpleNamespace(Variant=Variant,VariantType=SimpleNamespace(new=lambda v:v))
            loader=SimpleNamespace(name='controlled-profiler')
            def exec_module(module):
                modules=copy.deepcopy(original['modules'])
                if scenario=='changed-module':modules['core']['sha256']='e'*64
                module.mutter_libraries_provenance=lambda pid:modules
            loader.exec_module=exec_module
            def output(args,**kwargs):
                if args[:2]==['pacman','-Qqo']:return 'gnome-shell\n'
                if args[:2]==['pacman','-Q']:return original['image']['shell_package' if args[2]=='gnome-shell' else 'mutter_package']+'\n'
                return 'GNOME Shell 51.0\n'
            with patch.dict(sys.modules,{'gi':SimpleNamespace(), 'gi.repository':SimpleNamespace(Gio=gio,GLib=glib)}), \
                 patch.object(reference.os,'getuid',return_value=1000), \
                 patch.object(reference,'image',return_value=original['image']), \
                 patch.object(reference,'protected',side_effect=protected), \
                 patch.object(reference,'process_start',return_value='222'), \
                 patch.object(reference.Path,'resolve',return_value=Path('/usr/bin/gnome-shell')), \
                 patch.object(reference.importlib.machinery,'SourceFileLoader',return_value=loader), \
                 patch.object(reference.importlib.util,'spec_from_loader',return_value=object()), \
                 patch.object(reference.importlib.util,'module_from_spec',return_value=SimpleNamespace()), \
                 patch.object(reference.subprocess,'check_output',side_effect=output):
                stream=io.StringIO()
                if scenario=='valid':
                    with redirect_stdout(stream):reference.main()
                    self.assertEqual(json.loads(stream.getvalue()),original)
                else:
                    with redirect_stdout(stream),self.assertRaises(ValueError):reference.main()
                    self.assertEqual(stream.getvalue(),'')

    def test_stock_stage_precedes_diagnostic_and_default_is_still_diagnostic(self):
        source=(ROOT/'packaging/marlin/perf/Containerfile.baseline').read_text()
        stock=source.split('AS stock-reference',1)[1].split('# Build the pinned',1)[0]
        self.assertNotIn('pacman -U',stock)
        self.assertNotIn('mutter-profiler-build',stock)
        self.assertIn('pacman -Qlq linux mutter gnome-shell',stock)
        self.assertIn('roost-perf-reference-image stock',stock)
        self.assertTrue(source.rsplit('FROM ',1)[1].startswith('${COHERENT_BASE} AS diagnostic-reference'))
        diagnostic=source.rsplit('FROM ',1)[1]
        self.assertIn("pacman -Q mutter | grep -qx 'mutter 51.0-1.6'",diagnostic)
        self.assertIn('sha256sum -c sha256sums',diagnostic)


if __name__=='__main__':unittest.main()
