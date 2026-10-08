#!/usr/bin/env python3
"""Controlled production helper policies; no genuine compositor/DBus/VM claim."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import socket
import tempfile
import unittest
from unittest.mock import patch

spec=importlib.util.spec_from_file_location('display_fixture',Path(__file__).with_name('roost-display-config-fixture.py'))
M=importlib.util.module_from_spec(spec);spec.loader.exec_module(M)


def state(serial=1,scale=1.0):
    names=('roost-0','Roost','Nested','')
    return (serial,[(names,[('1280x800@60.000',1280,800,60.0,1.0,[1.0,1.25],{})],{})],
            [(0,0,scale,0,True,[names],{})],{})


class Transport:
    def __init__(self):
        self.owner=':1.8';self.calls=[];self.state=state();self.after_state=None;self.on_apply=None
    def __call__(self,dest,path,interface,method,signature,args,expected,budget):
        self.calls.append((dest,method,args,expected))
        if method=='GetNameOwner':return (self.owner,)
        if method=='GetConnectionUnixProcessID':return (34,)
        if method=='GetConnectionUnixUser':return (1000,)
        if method=='GetCurrentState':
            result=copy.deepcopy(self.state)
            if self.after_state:self.after_state()
            return result
        if method=='ApplyMonitorsConfig':
            if self.on_apply:self.on_apply(args)
            return ()
        raise AssertionError('unexpected transport')


class DisplayFixture(unittest.TestCase):
    def original(self):
        principal={'pid':34,'uid':1000,'start_ticks':57,'executed_key':[1,2,1000,0o100755,100,4,5],
                   'socket_key':[1,3,1000,0o140755,0,4,5],'session_bus_matches':True,'state_route_matches':True}
        return {'schema':1,'principal':principal,'bus':{'owner':':1.8','pid':34,'uid':1000},
                'physical_output_sha256':M.current_serial(state(),1280,800)[1]}
    def apply(self,bus=None,original=None,observe=None,method=2,scale=1.25):
        bus=bus or Transport();original=original or self.original()
        result=M.apply(original,observe or (lambda:copy.deepcopy(original['principal'])),bus,1280,800,method,scale)
        return result,bus
    def test_current_serial_is_read_for_each_original_request_including_restore(self):
        first,bus=self.apply();self.assertEqual(first['serial'],1)
        bus.state=state(2,1.25)
        restored,_=self.apply(bus=bus,method=1,scale=1.0)
        applies=[x for x in bus.calls if x[1]=='ApplyMonitorsConfig']
        self.assertEqual([x[2][0] for x in applies],[1,2])
        self.assertEqual([x[2][1:3] for x in applies],[(2,[(0,0,1.25,0,True,[('roost-0','1280x800@60.000',{})])]),(1,[(0,0,1.0,0,True,[('roost-0','1280x800@60.000',{})])])])
        self.assertTrue(all(x[0]==':1.8' for x in bus.calls if x[1] in ('GetCurrentState','ApplyMonitorsConfig')))
    def test_serial_unknown_bool_zero_overflow_or_malformed_never_applies(self):
        for value in (True,False,0,-1,2**32,'1',1.0):
            bus=Transport();bus.state=state(value)
            with self.subTest(value=value),self.assertRaises(ValueError):self.apply(bus)
            self.assertFalse(any(x[1]=='ApplyMonitorsConfig' for x in bus.calls))
        for value in ((),(1,),[1,[],[],{}]):
            with self.subTest(shape=value),self.assertRaises(ValueError):M.current_serial(value,1280,800)
    def test_current_original_connector_mode_and_geometry_required(self):
        mutations=[lambda v:v[1].clear(),lambda v:v[1].append(v[1][0]),
                   lambda v:v[1].__setitem__(0,(('other','Roost','Nested',''),v[1][0][1],{})),
                   lambda v:v[1][0][1].__setitem__(0,('1280x800@60.000',1279,800,60.,1.,[],{})),
                   lambda v:v[1][0][1].__setitem__(0,('other',1280,800,60.,1.,[],{})),
                   lambda v:v[2].__setitem__(0,(1,0,1.,0,True,v[2][0][5],{})),
                   lambda v:v[2].__setitem__(0,(0,0,float('nan'),0,True,v[2][0][5],{})),
                   lambda v:v[2].__setitem__(0,(0,0,1.,1,True,v[2][0][5],{})),
                   lambda v:v[2].__setitem__(0,(0,0,1.,0,False,v[2][0][5],{}))]
        for mutate in mutations:
            bus=Transport();mutate(bus.state)
            with self.assertRaises(ValueError):self.apply(bus)
            self.assertFalse(any(x[1]=='ApplyMonitorsConfig' for x in bus.calls))
    def test_physical_identity_change_refused_even_when_connector_and_mode_match(self):
        bus=Transport();bus.state[1][0]=( ('roost-0','Changed','Nested',''),bus.state[1][0][1],{})
        with self.assertRaises(ValueError):self.apply(bus)
        self.assertFalse(any(x[1]=='ApplyMonitorsConfig' for x in bus.calls))
    def test_owner_replacement_before_and_between_read_and_apply_refused(self):
        for before in (True,False):
            bus=Transport()
            if before:bus.owner=':1.9'
            else:bus.after_state=lambda:setattr(bus,'owner',':1.9')
            with self.assertRaises(ValueError):self.apply(bus)
            self.assertFalse(any(x[1]=='ApplyMonitorsConfig' for x in bus.calls))
    def test_original_start_UID_executable_or_socket_change_refused(self):
        original=self.original()
        for field,new in [('start_ticks',58),('uid',1001),('executed_key',[1,9,1000,0o100755,100,4,5]),('socket_key',[1,9,1000,0o140755,0,4,5])]:
            actual=copy.deepcopy(original['principal']);actual[field]=new;bus=Transport()
            with self.assertRaises(ValueError):self.apply(bus,original,lambda:actual)
            self.assertFalse(any(x[1]=='ApplyMonitorsConfig' for x in bus.calls))
    def test_stale_apply_error_preserved_without_retry_or_second_query(self):
        bus=Transport();failure=RuntimeError('controlled stale refusal')
        def refuse(_):raise failure
        bus.on_apply=refuse
        with self.assertRaises(RuntimeError) as caught:self.apply(bus)
        self.assertIs(caught.exception,failure)
        self.assertEqual(sum(x[1]=='ApplyMonitorsConfig' for x in bus.calls),1)
        self.assertEqual(sum(x[1]=='GetCurrentState' for x in bus.calls),1)
    def test_post_apply_owner_loss_withholds_success(self):
        bus=Transport();bus.on_apply=lambda _:setattr(bus,'owner',':1.9')
        with self.assertRaises(ValueError):self.apply(bus)
        self.assertEqual(sum(x[1]=='ApplyMonitorsConfig' for x in bus.calls),1)
    def test_original_receipt_FD_permissions_schema_and_path_substitution(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'receipt';original=self.original();M.write_original(path,original)
            self.assertEqual(M.read_original(path),original)
            with self.assertRaises(FileExistsError):M.write_original(path,original)
            path.chmod(0o644)
            with self.assertRaises(ValueError):M.read_original(path)
            path.chmod(0o600);real=os.fstat;calls=0
            def replaced(fd):
                nonlocal calls
                value=real(fd);calls+=1
                if calls==2:
                    path.rename(path.with_name('old'));path.write_text(json.dumps(original));path.chmod(0o600)
                return value
            with patch.object(M.os,'fstat',side_effect=replaced),self.assertRaises(ValueError):M.read_original(path)
    def test_original_receipt_rejects_bool_uid_keys_unknown_private_fields(self):
        mutations=[lambda v:v.update(schema=True),lambda v:v.update(private='private value'),
                   lambda v:v['principal'].update(uid=True),lambda v:v['principal'].update(start_ticks=0),
                   lambda v:v['principal']['socket_key'].__setitem__(4,False),
                   lambda v:v['bus'].update(pid=35),lambda v:v['bus'].update(uid=True)]
        with tempfile.TemporaryDirectory() as directory:
            for index,mutate in enumerate(mutations):
                path=Path(directory)/str(index);original=self.original();mutate(original);M.write_original(path,original)
                with self.assertRaises(ValueError):M.read_original(path)

    def test_error_decoder_has_fixed_stage_and_kind_without_private_exception_body(self):
        with patch('sys.stderr') as stderr:
            with self.assertRaises(SystemExit) as caught:M.main(['private invalid arguments'])
        self.assertEqual(caught.exception.code,1)
        emitted=''.join(x.args[0] for x in stderr.write.call_args_list if x.args)
        self.assertEqual(json.loads(emitted),{'stage':'arguments','error':'rejected'})


class OriginalProcessPolicies(unittest.TestCase):
    def observe(self, mutation=None):
        # Controlled files replace /proc metadata in this policy seam. No
        # synthetic process can qualify a real compositor runtime receipt.
        with tempfile.TemporaryDirectory() as directory:
            home=Path(directory);proc=home/'proc';proc.mkdir();runtime=home/'runtime';runtime.mkdir(mode=0o700)
            executable=home/'compositor';executable.write_text('controlled metadata only');executable.chmod(0o755)
            (proc/'exe').symlink_to(executable)
            (proc/'stat').write_text('34 (controlled process) '+' '.join(['S']+['0']*18+['57']))
            environment={'XDG_RUNTIME_DIR':str(runtime),'DBUS_SESSION_BUS_ADDRESS':'controlled-bus','ROOST_COMPOSITOR_STATE':'controlled-state'}
            (proc/'environ').write_bytes(b'DBUS_SESSION_BUS_ADDRESS=controlled-bus\0ROOST_COMPOSITOR_STATE=controlled-state\0')
            with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as endpoint:
                endpoint.bind(str(runtime/'original'))
                if mutation:mutation(home,proc,executable,runtime)
                with patch.object(M,'Path',side_effect=lambda value:proc if str(value)=='/proc/34' else Path(value)),patch.dict(M.os.environ,environment):
                    return M.process(34,executable,'original')
    def test_original_actual_FD_named_mapped_and_socket_metadata_agree(self):
        value=self.observe();self.assertEqual(value['pid'],34);self.assertEqual(value['start_ticks'],57)
        self.assertTrue(value['session_bus_matches'] and value['state_route_matches'])
    def test_changed_mapped_executable_or_missing_original_socket_refuses(self):
        def executable_change(home,proc,exe,runtime):
            other=home/'other';other.write_text('other metadata');(proc/'exe').unlink();(proc/'exe').symlink_to(other)
        for mutation in (executable_change,lambda home,proc,exe,runtime:(runtime/'original').unlink()):
            with self.assertRaises((ValueError,OSError)):self.observe(mutation)
    def test_private_runtime_and_original_environment_route_required(self):
        for mutation in (lambda home,proc,exe,runtime:runtime.chmod(0o755),
                         lambda home,proc,exe,runtime:(proc/'environ').write_bytes(b'DBUS_SESSION_BUS_ADDRESS=other\0ROOST_COMPOSITOR_STATE=controlled-state\0')):
            with self.assertRaises(ValueError):self.observe(mutation)
    def test_changed_named_executable_during_original_FD_read_refuses(self):
        real=os.fstat;calls=0
        def replaced(fd):
            nonlocal calls
            value=real(fd);calls+=1
            if calls==2:
                name=Path(os.readlink(f'/proc/self/fd/{fd}'))
                name.rename(name.with_name('old'));name.write_text('replacement metadata')
            return value
        with patch.object(M.os,'fstat',side_effect=replaced),self.assertRaises(ValueError):self.observe()


class PortalEntrypoints(unittest.TestCase):
    def test_original_portal_wrapper_direct_entries_all_have_executable_shebang(self):
        # Run the actual wrapper; controlled podman checks each mounted command's
        # Unix launchability and syntax. No container/product/provider is run.
        root=Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as directory:
            home=Path(directory);candidate=home/'candidate/usr/bin';candidate.mkdir(parents=True)
            for name in ('roost-compositor','roost-shell-gtk'):
                p=candidate/name;p.write_text('#!/bin/sh\nexit 0\n');p.chmod(0o755)
            tools=home/'tools';tools.mkdir();tool=tools/'podman'
            tool.write_text('''#!/usr/bin/env python3
import os,sys,pathlib,subprocess
if sys.argv[1]=='build':sys.exit(0)
entry=sys.argv[-1]
assert entry.startswith('/repo/tests/portal-reference/')
p=pathlib.Path(os.environ['CONTROLLED_SOURCE_ROOT'])/entry.removeprefix('/repo/')
if not os.access(p,os.X_OK):sys.exit(126)
assert p.read_bytes().startswith(b'#!/usr/bin/env bash\\n')
subprocess.run(['bash','-n',str(p)],check=True)
with open(os.environ['ENTRYPOINT_LOG'],'a') as output:output.write(entry+'\\n')
''');tool.chmod(0o755);log=home/'entries'
            env=dict(os.environ,PATH=str(tools)+os.pathsep+os.environ['PATH'],CONTROLLED_SOURCE_ROOT=str(root),ENTRYPOINT_LOG=str(log))
            result=subprocess.run([str(root/'scripts/roost-portal-security'),'--candidate',str(home/'candidate'),'--out',str(home/'out')],env=env,capture_output=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(log.read_text().splitlines(),['/repo/tests/portal-reference/run.sh','/repo/tests/portal-reference/settings-run.sh','/repo/tests/portal-reference/global-shortcuts-run.sh','/repo/tests/portal-reference/backgrounds-run.sh'])


if __name__=='__main__':unittest.main()
