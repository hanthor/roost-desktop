#!/usr/bin/python3
"""Production policy/transport negatives only; never native VM qualification."""
import base64
import copy
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[2]
def load(name,path):
    loader=importlib.machinery.SourceFileLoader(name,str(path));spec=importlib.util.spec_from_loader(name,loader)
    module=importlib.util.module_from_spec(spec);loader.exec_module(module);return module
H=load('picker_host_tests',ROOT/'scripts/lib/native_picker_pilot.py')
G=load('picker_guest_tests',ROOT/'packaging/marlin/vm-lane/roost-vm-gtk-picker-pilot')
T=load('picker_transport_tests',ROOT/'scripts/lib/vm_guest_agent.py')
F=load('picker_files_tests',ROOT/'scripts/lib/native-files-pilot-tests.py')

def evidence(phase):
    base=F.original_evidence()
    front=dict(base['compositor'],pid=90,owner=':1.90');back=dict(front,pid=91,owner=':1.91')
    original={'base':base,'frontend':front,'backend':back,'route_sha256':'c'*64,'metadata_sha256':'d'*64,
              'packages':{k:k+' 51.0-1' for k in ('gtk4','nautilus','xdg-desktop-portal','xdg-desktop-portal-gnome','python-gobject','python-atspi','roost')},
              'fixture':'/run/user/1000/roost-native-picker-abc_1234','seed_sha256':H.SHA,'caller_script_sha256':'e'*64}
    value={'phase':phase,'ready':True,'scope':H.SCOPE,'original':original,'principals_retained':True,'filesystem_unchanged':True}
    if phase=='start':return value
    caller={'pid':78,'uid':1000,'start_ticks':15,'exe_sha256':'f'*64}
    grant=phase in ('granted','cancel-dialog','dismissed','restored-clicked','restored-tested','closed');dismiss=phase in ('dismissed','restored-clicked','restored-tested','closed')
    calls=2 if phase in ('cancel-dialog','dismissed','restored-clicked','restored-tested','closed') else 0 if phase in ('parent','parent-clicked','parent-tested') else 1
    callbacks=2 if dismiss else 1 if grant else 0
    clicks=0 if phase=='parent' else 2 if phase in ('restored-clicked','restored-tested','closed') else 1
    keys=0 if phase in ('parent','parent-clicked') else 2 if phase in ('restored-tested','closed') else 1
    value.update(caller=caller,caller_provider=dict(caller,owner=':2.78'),parent_id=2,parent_rect=[100,50,1100,720],ui_sequence=1,
                 open_count=calls,parent_clicks=clicks,parent_keys=keys,portal_calls=calls,portal_responses=callbacks,callback_count=callbacks,
                 granted=grant,dismissed=dismiss,uri_matches=grant,read_sha256=H.SHA if grant else None)
    if phase in ('grant-dialog','blocked','selected','cancel-dialog'):
        value.update(dialog_id=3,dialog_rect=[250,150,800,500],target=[280,210,160,32] if phase!='blocked' else [112,62,100,40],selected=phase=='selected',nautilus_provider=base['accessibility']['provider'])
    elif phase in ('parent','parent-clicked','parent-tested','granted','dismissed','restored-clicked','restored-tested'):value.update(open_target=[112,126,180,40],probe_target=[112,62,100,40])
    else:value.update(restored_layout=base['original_layout'],restored_focus=1,restored_workspace=0,cleanup=True)
    return value


class Policy(unittest.TestCase):
    def test_all_positive_finite_envelopes(self):
        for phase in H.PHASES:
            with self.subTest(phase=phase):H.validate(evidence(phase),phase)
    def test_minimal_ready_is_not_proof(self):
        for phase in H.PHASES:
            with self.subTest(phase=phase),self.assertRaises(RuntimeError):H.validate({'phase':phase,'ready':True},phase)
    def test_each_required_evidence_field(self):
        for phase in H.PHASES:
            for field in evidence(phase):
                value=evidence(phase);del value[field]
                with self.subTest(phase=phase,field=field),self.assertRaises(RuntimeError):H.validate(value,phase)
    def test_unknown_evidence_not_retained(self):
        value=evidence('parent');value['private_text']='secret'
        with self.assertRaises(RuntimeError):H.validate(value,'parent')
    def test_no_response_or_no_callback_cannot_grant(self):
        for field in ('open_count','portal_calls','portal_responses','callback_count'):
            value=evidence('granted');value[field]=0
            with self.subTest(field=field),self.assertRaises(RuntimeError):H.validate(value,'granted')
    def test_grant_requires_real_uri_and_bytes(self):
        for field,bad in [('granted',False),('uri_matches',False),('read_sha256','0'*64)]:
            value=evidence('granted');value[field]=bad
            with self.subTest(field=field),self.assertRaises(RuntimeError):H.validate(value,'granted')
    def test_user_dismiss_cannot_be_fake_success(self):
        value=evidence('dismissed');value['dismissed']=False
        with self.assertRaises(RuntimeError):H.validate(value,'dismissed')
    def test_parent_input_leak_rejected(self):
        for field in ('parent_clicks','parent_keys'):
            value=evidence('blocked');value[field]=2
            with self.subTest(field=field),self.assertRaises(RuntimeError):H.validate(value,'blocked')
    def test_provider_kernel_principal_mismatch(self):
        for who in ('caller_provider','nautilus_provider'):
            value=evidence('grant-dialog');value[who]=dict(value[who],pid=999)
            with self.subTest(who=who),self.assertRaises(RuntimeError):H.validate(value,'grant-dialog')
    def test_original_services_and_caller_cannot_change(self):
        initial=evidence('start')['original'];value=evidence('parent');value['original']['frontend']['owner']=':1.222'
        with self.assertRaises(RuntimeError):H.validate(value,'parent',initial)
        value=evidence('parent')
        with self.assertRaises(RuntimeError):H.validate(value,'parent',caller=dict(value['caller'],start_ticks=16))
    def test_parent_recycled_or_old_window_rejected(self):
        for identifier in (True,1,4):
            value=evidence('parent');value['parent_id']=identifier
            with self.subTest(identifier=identifier),self.assertRaises(RuntimeError):H.validate(value,'parent',parent=2)
    def test_selected_requires_real_selected_state(self):
        value=evidence('selected');value['selected']=False
        with self.assertRaises(RuntimeError):H.validate(value,'selected')
    def test_bool_restore_types_are_not_integers(self):
        for field,bad in [('restored_focus',True),('restored_workspace',False),('restored_layout',[[True,[False,0,100,100]]])]:
            value=evidence('closed');value[field]=bad
            with self.subTest(field=field),self.assertRaises(RuntimeError):H.validate(value,'closed')
    def test_cleanup_and_original_layout_required(self):
        for field,bad in [('cleanup',False),('restored_focus',None),('restored_workspace',1),('restored_layout',[[1,[0,0,101,100]]])]:
            value=evidence('closed');value[field]=bad
            with self.subTest(field=field),self.assertRaises(RuntimeError):H.validate(value,'closed')
    def test_fixed_ordinary_host_scope(self):
        value=evidence('granted');value['scope']='sandbox grant'
        with self.assertRaises(RuntimeError):H.validate(value,'granted')
    def test_request_actual_default_portal_fields(self):
        directory='/run/user/1000/roost-native-picker-abc_1234'
        options={'handle_token':'gtk123','modal':True,'current_folder':list(os.fsencode(directory)+b'\0')}
        self.assertEqual(H.request_fields('wayland:abc','known',options,directory,'known'),'gtk123')
        for field,bad in [('handle_token','../bad'),('modal',1),('current_folder',[True]),('current_folder',[0]*4097),('multiple',True),('directory',True)]:
            altered=dict(options);altered[field]=bad
            with self.subTest(field=field,bad=str(bad)[:8]),self.assertRaises(RuntimeError):H.request_fields('wayland:abc','known',altered,directory,'known')
    def test_empty_or_wrong_parent_refused(self):
        for parent in ('wayland:','x11:1',None,'wayland:'+'a'*513):
            with self.subTest(parent=str(parent)[:12]),self.assertRaises(RuntimeError):H.request_fields(parent,'known',{},'/tmp','known')
    def test_response_exact_known_uri_and_user_cancel(self):
        uri='file:///known%20seed%20%2525.txt'
        H.response_fields(0,{'uris':[uri]},uri);H.response_fields(1,{'uris':[]},uri)
        for code,result in [(True,{}),(2,{}),(0,{}),(0,{'uris':['file:///private']}),(1,{'uris':[uri]}),(0,{'uris':[uri,uri]})]:
            with self.subTest(code=code,result=result),self.assertRaises(RuntimeError):H.response_fields(code,result,uri)
    def test_selection_precedence_original_frontend_environment(self):
        env={'HOME':'/home/roost-test','XDG_CURRENT_DESKTOP':'Roost:GNOME'}
        with patch.object(G.pwd,'getpwuid',return_value=type('Owner',(),{'pw_dir':'/home/roost-test'})()):
            paths=G.selection_paths(env)
            self.assertEqual(str(paths[0]),'/home/roost-test/.config/xdg-desktop-portal/roost-portals.conf')
            self.assertLess(paths.index(Path('/etc/xdg/xdg-desktop-portal/portals.conf')),paths.index(Path('/usr/share/xdg-desktop-portal/roost-portals.conf')))
            for bad in [dict(env,XDG_DESKTOP_PORTAL_DIR='/tmp'),dict(env,XDG_CURRENT_DESKTOP='GNOME'),dict(env,XDG_CONFIG_DIRS=':'.join('/tmp' for _ in range(9))),dict(env,XDG_DATA_DIRS='../private')]:
                with self.assertRaises(RuntimeError):G.selection_paths(bad)
    def test_transport_errors_fixed_requested_phase(self):
        for diagnostic,accepted in [({'phase':'granted','exception_type':'RuntimeError'},True),({'phase':'granted','exception_type':'RuntimeError','message':'private'},False),({'phase':[],'exception_type':'RuntimeError'},False),({'phase':'granted','exception_type':{}},False)]:self.assertEqual(T.valid_native_picker_error(diagnostic),accepted)
    def test_transport_failed_payload_privacy(self):
        def execute(body):
            agent=object.__new__(T.GuestAgent);agent.command=lambda name,**kw:{'pid':1} if name=='guest-exec' else {'exited':True,'exitcode':9,'out-data':base64.b64encode(json.dumps(body).encode()).decode()}
            with self.assertRaises(RuntimeError) as caught:agent.run('native-picker','granted')
            return str(caught.exception)
        self.assertIn('"phase": "granted"',execute({'native_picker_error':{'phase':'granted','exception_type':'RuntimeError'}}))
        for body in [{'native_picker_error':{'phase':'dismissed','exception_type':'RuntimeError'}},{'native_picker_error':{'phase':'granted','exception_type':'RuntimeError','message':'private secret'}}]:
            message=execute(body);self.assertNotIn('private',message);self.assertNotIn('dismissed',message);self.assertIn('exit=9',message)
    def test_balanced_host_journey_uses_actual_phase_admission(self):
        inputs=[]
        class Agent:
            sequence=0
            def run(self,action,phase,**_):
                self.sequence+=1;value=evidence(phase)
                if phase!='start':value['ui_sequence']=self.sequence
                self.action=action;return value
        class Qmp:
            def keys(self,*chord):inputs.append(('keys',chord))
            def click(self,x,y):inputs.append(('click',x,y))
            def frame(self,_):pass
        results=[]
        with tempfile.TemporaryDirectory() as directory:H.run(Qmp(),Agent(),directory,lambda *args:results.append(args))
        self.assertEqual([v for v in inputs if v[0]=='keys'],[('keys',('f8',)),('keys',('f8',)),('keys',('ret',)),('keys',('esc',)),('keys',('f8',)),('keys',('ctrl','w'))])
        self.assertEqual(len([v for v in inputs if v[0]=='click']),6);self.assertIs(results[-1][1],True)
    def test_failure_never_records_success(self):
        class Agent:
            def run(self,*_,**__):return {'phase':'start','ready':True}
        results=[]
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(RuntimeError):H.run(None,Agent(),directory,lambda *v:results.append(v))
            self.assertEqual(json.loads((Path(directory)/'native-gtk-picker/failure.json').read_text()),{'exception_type':'RuntimeError'})
        self.assertIs(results[-1][1],False)
    def test_pre_open_inactive_listeners_refused_by_production_envelope(self):
        for phase,field in [('parent-clicked','parent_clicks'),('parent-tested','parent_keys')]:
            value=evidence(phase);value[field]=0
            with self.subTest(phase=phase),self.assertRaises(RuntimeError):H.validate(value,phase)
    def test_blocked_requires_exact_proven_one_one(self):
        for field in ('parent_clicks','parent_keys'):
            for bad in (0,2,True):
                value=evidence('blocked');value[field]=bad
                with self.subTest(field=field,bad=bad),self.assertRaises(RuntimeError):H.validate(value,'blocked')
    def test_post_dismiss_missing_positive_or_regression_refused(self):
        for phase,field,bad in [('restored-clicked','parent_clicks',1),('restored-tested','parent_keys',1),('restored-tested','parent_clicks',1),('closed','parent_keys',0)]:
            value=evidence(phase);value[field]=bad
            with self.subTest(phase=phase,field=field),self.assertRaises(RuntimeError):H.validate(value,phase)
    def test_actual_host_path_stops_before_next_action_on_bad_probe(self):
        for rejected,field,bad in [('parent-clicked','parent_clicks',0),('parent-tested','parent_keys',0),('blocked','parent_keys',2),('restored-clicked','parent_clicks',1),('restored-tested','parent_keys',1)]:
            inputs=[];results=[]
            class Agent:
                sequence=0
                def run(self,_,phase,**__):
                    self.sequence+=1;value=evidence(phase)
                    if phase!='start':value['ui_sequence']=self.sequence
                    if phase==rejected:value[field]=bad
                    return value
            class Qmp:
                def keys(self,*chord):inputs.append(chord)
                def click(self,*_):inputs.append(('click',))
                def frame(self,_):pass
            with self.subTest(phase=rejected),tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(RuntimeError):H.run(Qmp(),Agent(),directory,lambda *v:results.append(v))
            self.assertIs(results[-1][1],False)
            self.assertNotIn(('ctrl','w'),inputs)
            if rejected.startswith('parent-'):self.assertNotIn(('ret',),inputs)
    def test_real_controlled_file_directory_and_original_bytes(self):
        with tempfile.TemporaryDirectory() as parent:
            directory=Path(parent)/'fixture';directory.mkdir(mode=0o700)
            seed=directory/H.SEED;seed.write_bytes(H.PAYLOAD);seed.chmod(0o600)
            value={'fixture':str(directory),'fixture_inode':[directory.stat().st_dev,directory.stat().st_ino],
                   'seed_inode':[seed.stat().st_dev,seed.stat().st_ino]}
            G.fixture(value)
            seed.write_bytes(b'changed')
            with self.assertRaises(RuntimeError):G.fixture(value)
    def test_cancel_or_unknown_filesystem_creation_is_fatal(self):
        with tempfile.TemporaryDirectory() as parent:
            directory=Path(parent)/'fixture';directory.mkdir(mode=0o700)
            seed=directory/H.SEED;seed.write_bytes(H.PAYLOAD);seed.chmod(0o600)
            value={'fixture':str(directory),'fixture_inode':[directory.stat().st_dev,directory.stat().st_ino],
                   'seed_inode':[seed.stat().st_dev,seed.stat().st_ino]}
            (directory/'cancelled-through-picker').mkdir()
            with self.assertRaises(RuntimeError):G.fixture(value)
    def test_original_seed_and_directory_replacement_fatal(self):
        with tempfile.TemporaryDirectory() as parent:
            directory=Path(parent)/'fixture';directory.mkdir(mode=0o700)
            seed=directory/H.SEED;seed.write_bytes(H.PAYLOAD);seed.chmod(0o600)
            value={'fixture':str(directory),'fixture_inode':[directory.stat().st_dev,directory.stat().st_ino],
                   'seed_inode':[seed.stat().st_dev,seed.stat().st_ino]}
            seed.rename(Path(parent)/'old-seed');seed.symlink_to(Path(parent)/'old-seed')
            with self.assertRaises(RuntimeError):G.fixture(value)
            directory.rename(Path(parent)/'original');directory.mkdir(mode=0o700)
            with self.assertRaises(RuntimeError):G.fixture(value)


if __name__=='__main__':unittest.main()
