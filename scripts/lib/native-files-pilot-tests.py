#!/usr/bin/python3
"""Pure production-path policy tests; no actual GTK/native qualification."""
import ast
from types import SimpleNamespace
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import copy
import base64
import os
import io
import json
from contextlib import redirect_stdout
import stat
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[2]
def load(name,path):
    loader=importlib.machinery.SourceFileLoader(name,str(path));spec=importlib.util.spec_from_loader(name,loader)
    module=importlib.util.module_from_spec(spec);loader.exec_module(module);return module
G=load('files_guest',ROOT/'packaging/marlin/vm-lane/roost-vm-nautilus-pilot')
H=load('files_host',ROOT/'scripts/lib/native_files_pilot.py')
T=load('files_transport',ROOT/'scripts/lib/vm_guest_agent.py')
def original_evidence():
    process={'pid':12,'uid':1000,'start_ticks':2,'exe_sha256':'a'*64,'owner':':1.2'}
    compositor=dict(process,pid=34,owner=':1.3')
    provider=dict(process,owner=':2.2')
    return {'nautilus':process,'compositor':compositor,
        'accessibility':{'provider':provider,'broker':{'pid':56,'uid':1000,'start_ticks':3,'exe_sha256':'b'*64},
            'transport_peer':{'pid':57,'uid':1000,'start_ticks':4,'exe_sha256':'c'*64},
            'socket':{'dev':1,'inode':2,'uid':1000,'mode':stat.S_IFSOCK|0o600}},
        'session':{'Id':'c2','VTNr':'1','User':'1000','Service':'greetd','Class':'user'},
        'original_windows':[1],'original_layout':[[1,[0,0,100,100]]],'original_focus':1,'original_workspace':0}


def evidence(phase,created=False):
    value={'phase':phase,'ready':True,'original':original_evidence(),'owner_scope':H.OWNER_SCOPE,
        'original_principals_retained':True,'filesystem':{'seed_sha256':H.SEED_SHA,H.CREATED:created or phase in ('created','cancelled','closed'),H.CANCELLED:False}}
    if phase=='start':value['packages']={n:f'{n} 51.0-1' for n in ('nautilus','python-atspi','roost')}
    elif phase=='closed':value.update(restored=True,restored_layout=value['original']['original_layout'],restored_focus=1,restored_workspace=0,
        cleanup={'fixture_inode_matched':True,'known_members_removed':True,'fixture_removed':True,'context_retained':True})
    else:
        value.update(window_id=2,rect=[100,100,800,600],node_count=10,showing_node_count=8,seed_cells=1,
            created_cells=int(value['filesystem'][H.CREATED]),cancelled_cells=0,
            dialog_count=int(phase in ('dialog','create-typed','cancel-typed')),focused_entry_count=int(phase in ('dialog','create-typed','cancel-typed')))
        if phase in ('create-typed','cancel-typed'):value['known_entry_text_matches']=True
    return value


class AgentEvidence:
    def __init__(self):self.created=False
    def run(self,_,phase,**kw):
        if phase=='created':self.created=True
        return evidence(phase,self.created)

class State:
    def __init__(self,showing):self.showing=showing
    def contains(self,_):return self.showing
class Node:
    def __init__(self,path,children=(),showing=False):self.path=path;self.children=children;self.showing=showing
    def clear_cache(self):pass
    @property
    def childCount(self):return len(self.children)
    def getState(self):return State(self.showing)
    def getChildAtIndex(self,index):return self.children[index]
class Policy(unittest.TestCase):
    def test_hidden_nodes_counted(self):
        root=Node('root',[Node(str(i),[Node(f'{i}-{j}') for j in range(255)]) for i in range(17)])
        with self.assertRaises(RuntimeError):G.walk_tree(root,lambda f:f(),0)
    def test_cycle_rejected(self):
        root=Node('root');root.children=[root]
        with self.assertRaises(RuntimeError):G.walk_tree(root,lambda f:f(),0)
    def test_exact_all_node_receipt(self):
        nodes,count=G.walk_tree(Node('r',[Node('hidden'),Node('shown',showing=True)]),lambda f:f(),0)
        self.assertEqual(count,3);self.assertEqual([v.path for v in nodes],['shown'])
    def test_depth_bound(self):
        root=Node('0');current=root
        for i in range(34):current.children=[Node(str(i+1))];current=current.children[0]
        with self.assertRaises(RuntimeError):G.walk_tree(root,lambda f:f(),0)
    def test_text_bound_before_read(self):
        class Text:
            characterCount=999999
            def getText(self,*_):self.fail_read=True;raise AssertionError('unbounded text read')
        class Entry:
            def queryText(self):return Text()
        with self.assertRaises(RuntimeError):G.known_text(Entry(),H.CANCELLED,lambda f:f())
    def test_exact_known_text_read(self):
        calls=[]
        class Text:
            characterCount=len(H.CANCELLED)
            def getText(self,start,end):calls.append((start,end));return H.CANCELLED
        class Entry:
            def queryText(self):return Text()
        self.assertTrue(G.known_text(Entry(),H.CANCELLED,lambda f:f()))
        self.assertEqual(calls,[(0,len(H.CANCELLED)+1)])
    def test_missing_parent_field_rejected(self):
        with self.assertRaises(RuntimeError):G.owned_window({'original_windows':[]},{'windows':[{'id':1,'app_id':G.APP}],'focused':1})
    def test_bounded_resource_refuses_symlink_and_oversize(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'seed';path.write_bytes(b'12345')
            with self.assertRaises(RuntimeError):G.bounded(path,4)
            alias=Path(directory)/'alias';alias.symlink_to(path)
            with self.assertRaises(OSError):G.bounded(alias,10)
    def test_deadline_refuses_stale_query(self):
        with patch.object(G,'DEADLINE',1),patch.object(G.time,'monotonic',return_value=2):
            with self.assertRaises(TimeoutError):G.deadline()
    def test_public_session_schema(self):
        value={'Id':'c2','VTNr':'1','User':str(G.UID),'Service':'greetd','Class':'user'}
        self.assertEqual(G.validate_session(value),value)
        for key,bad in (('Id','../../private'),('VTNr','2'),('User','100000'),('Class','manager')):
            with self.subTest(key=key),self.assertRaises(RuntimeError):G.validate_session(dict(value,**{key:bad}))
    def test_geometry_restoration_bounds(self):
        self.assertEqual(G.layout({'windows':[{'id':2,'rect':[0,0,100,100]},{'id':1,'rect':[1,1,99,99]}]}),[[1,[1,1,99,99]],[2,[0,0,100,100]]])
        with self.assertRaises(RuntimeError):G.layout({'windows':[{'id':True,'rect':[0,0,100,100]}]})
    def test_transport_refuses_arbitrary_error_body(self):
        good={'phase':'created','stage':'cells','exception_type':'RuntimeError'}
        self.assertTrue(T.valid_native_files_error(good))
        for candidate in (dict(good,message='private content'),dict(good,phase=[]),dict(good,exception_type='private body'),dict(good,stage='private body'),dict(good,stage=[]),{k:v for k,v in good.items() if k!='stage'}):
            self.assertFalse(T.valid_native_files_error(candidate))
    def test_actual_failure_stage_preserves_exit_without_private_error_text(self):
        for stage in ('context','window','tree'):
            original=RuntimeError('private filename and credential must never be retained')
            stream=io.StringIO()
            with patch.object(G.sys,'argv',['fixed-probe','mapped']), redirect_stdout(stream):
                if stage=='context':
                    with patch.object(G,'context',side_effect=original):status=G.main()
                else:
                    with patch.object(G,'context',return_value={}),patch.object(G,'guard',return_value={}), \
                         patch.object(G,'owned_window',side_effect=original if stage=='window' else None,return_value={}), \
                         patch.object(G,'accessibility',side_effect=original):status=G.main()
            self.assertEqual(status,1)
            diagnostic=json.loads(stream.getvalue())
            self.assertEqual(diagnostic,{'native_files_error':{'phase':'mapped','stage':stage,'exception_type':'RuntimeError'}})
            self.assertTrue(T.valid_native_files_error(diagnostic['native_files_error']))
            self.assertNotIn('private',stream.getvalue())
    def test_guard_stage_retained_on_failure_restored_on_success(self):
        value={'nautilus':1,'compositor':2,'service_channel':3,'session':4}
        with patch.object(G,'fast_guard',return_value={}),patch.object(G,'principal',side_effect=[1,2,3]), \
             patch.object(G,'login_session',return_value=4),patch.object(G,'original_app_route',side_effect=RuntimeError('private route')):
            G.error_stage('tree')
            with self.assertRaises(RuntimeError):G.guard(value)
            self.assertEqual(G.ERROR_STAGE,'guard-route')
        with patch.object(G,'fast_guard',return_value={}),patch.object(G,'principal',side_effect=[1,2,3]), \
             patch.object(G,'login_session',return_value=4),patch.object(G,'original_app_route'), \
             patch.object(G,'check_accessibility'),patch.object(G,'fixture_identity'):
            G.error_stage('tree');G.guard(value)
            self.assertEqual(G.ERROR_STAGE,'tree')
    def test_tree_stage_names_provider_lookup_and_subtree_walk(self):
        value={'fixture':'/owner/seed','accessibility':{'provider':{'owner':':1.2'}}}
        node=SimpleNamespace(name='seed',getState=lambda:SimpleNamespace(contains=lambda _state:True))
        app=SimpleNamespace(childCount=1,getChildAtIndex=lambda _index:node)
        api=SimpleNamespace(STATE_SHOWING=object())
        with patch.object(G,'fast_guard',return_value=None):
            with patch.object(G,'provider_app',side_effect=RuntimeError('fixed provider')):
                with self.assertRaises(RuntimeError):G.accessibility(value)
                self.assertEqual(G.ERROR_STAGE,'tree-provider')
            with patch.object(G,'provider_app',return_value=(app,api,None)), \
                 patch.object(G,'walk_tree',side_effect=RuntimeError('fixed walk')):
                with self.assertRaises(RuntimeError):G.accessibility(value)
                self.assertEqual(G.ERROR_STAGE,'tree-walk')
    def test_tree_walk_transient_retried_then_succeeds(self):
        calls=[]
        def flaky(_original,_phase):
            calls.append(True)
            if len(calls)==1:
                G.error_stage('tree-walk')
                raise RuntimeError('transient mid-walk inconsistency')
            return {'phase':'mapped'}
        stream=io.StringIO()
        with patch.object(G.sys,'argv',['fixed-probe','mapped']),patch.object(G,'context',return_value={}), \
             patch.object(G,'inspect',side_effect=flaky),redirect_stdout(stream):
            self.assertEqual(G.main(),0)
        self.assertEqual(len(calls),2)
        self.assertEqual(json.loads(stream.getvalue()),{'phase':'mapped'})
    def test_provider_enumerate_transient_retried_then_succeeds(self):
        calls=[]
        def flaky(_original,_phase):
            calls.append(True)
            if len(calls)==1:
                G.error_stage('provider-enumerate')
                raise RuntimeError('transient provider lookup inconsistency')
            return {'phase':'mapped'}
        stream=io.StringIO()
        with patch.object(G.sys,'argv',['fixed-probe','mapped']),patch.object(G,'context',return_value={}), \
             patch.object(G,'inspect',side_effect=flaky),redirect_stdout(stream):
            self.assertEqual(G.main(),0)
        self.assertEqual(len(calls),2)
        self.assertEqual(json.loads(stream.getvalue()),{'phase':'mapped'})
    def test_start_provider_enumerate_transient_retried_then_succeeds(self):
        calls=[]
        def flaky(_provisional,_query):
            calls.append(True)
            if len(calls)==1:
                G.error_stage('provider-enumerate')
                raise RuntimeError('transient provider lookup inconsistency')
            return ({'app':1},{'api':1},{'receipt':1})
        with patch.object(G,'provider_app',side_effect=flaky):
            self.assertEqual(G.start_provider({}),(({'app':1}),({'api':1}),({'receipt':1})))
        self.assertEqual(len(calls),2)
    def test_start_provider_enumerate_persistent_failure_finite(self):
        calls=[]
        def broken(_provisional,_query):
            calls.append(True)
            G.error_stage('provider-enumerate')
            raise RuntimeError('persistent provider lookup inconsistency')
        with patch.object(G,'provider_app',side_effect=broken):
            with self.assertRaises(RuntimeError):G.start_provider({})
        self.assertEqual(len(calls),G.TREE_WALK_ATTEMPTS)
    def test_start_provider_receipt_failure_never_retried(self):
        calls=[]
        def broken(_provisional,_query):
            calls.append(True)
            G.error_stage('provider-receipt')
            raise RuntimeError('private receipt body')
        with patch.object(G,'provider_app',side_effect=broken):
            with self.assertRaises(RuntimeError):G.start_provider({})
        self.assertEqual(len(calls),1)
    def test_provider_receipt_failure_never_retried(self):
        calls=[]
        def broken(_original,_phase):
            calls.append(True)
            G.error_stage('provider-receipt')
            raise RuntimeError('private receipt body')
        stream=io.StringIO()
        with patch.object(G.sys,'argv',['fixed-probe','mapped']),patch.object(G,'context',return_value={}), \
             patch.object(G,'inspect',side_effect=broken),redirect_stdout(stream):
            self.assertEqual(G.main(),1)
        self.assertEqual(len(calls),1)
        diagnostic=json.loads(stream.getvalue())
        self.assertEqual(diagnostic,{'native_files_error':{'phase':'mapped','stage':'provider-receipt','exception_type':'RuntimeError'}})
        self.assertTrue(T.valid_native_files_error(diagnostic['native_files_error']))
    def test_tree_walk_persistent_failure_keeps_finite_diagnostic(self):
        calls=[]
        def broken(_original,_phase):
            calls.append(True)
            G.error_stage('tree-walk')
            raise RuntimeError('private mid-walk body must never survive')
        stream=io.StringIO()
        with patch.object(G.sys,'argv',['fixed-probe','mapped']),patch.object(G,'context',return_value={}), \
             patch.object(G,'inspect',side_effect=broken),redirect_stdout(stream):
            self.assertEqual(G.main(),1)
        self.assertEqual(len(calls),G.TREE_WALK_ATTEMPTS)
        diagnostic=json.loads(stream.getvalue())
        self.assertEqual(diagnostic,{'native_files_error':{'phase':'mapped','stage':'tree-walk','exception_type':'RuntimeError'}})
        self.assertTrue(T.valid_native_files_error(diagnostic['native_files_error']))
        self.assertNotIn('private',stream.getvalue())
    def test_non_tree_walk_failure_never_retried(self):
        calls=[]
        def broken(_original,_phase):
            calls.append(True)
            G.error_stage('window')
            raise RuntimeError('private window body')
        stream=io.StringIO()
        with patch.object(G.sys,'argv',['fixed-probe','mapped']),patch.object(G,'context',return_value={}), \
             patch.object(G,'inspect',side_effect=broken),redirect_stdout(stream):
            self.assertEqual(G.main(),1)
        self.assertEqual(len(calls),1)
        self.assertEqual(json.loads(stream.getvalue()),{'native_files_error':{'phase':'mapped','stage':'window','exception_type':'RuntimeError'}})
    def test_original_files_route_not_private_env(self):
        value={'nautilus':{'pid':12},'compositor':{'pid':34}}
        with patch.object(G,'bounded',return_value=b'PRIVATE=never retained\0WAYLAND_DISPLAY=roost-nested-34\0'):
            G.original_app_route(value)
        with patch.object(G,'bounded',return_value=b'WAYLAND_DISPLAY=parent-host\0'):
            with self.assertRaises(RuntimeError):G.original_app_route(value)
    def test_transport_diagnostic_matches_requested_phase(self):
        for phase in ('mapped','created'):
            agent=object.__new__(T.GuestAgent)
            status={'exited':True,'exitcode':9,'out-data':base64.b64encode(__import__('json').dumps({'native_files_error':{'phase':phase,'stage':'cells','exception_type':'RuntimeError'}}).encode()).decode()}
            agent.command=lambda method,**kw:{'pid':123} if method=='guest-exec' else status
            with self.assertRaises(RuntimeError) as raised:agent.run('native-files','created')
            if phase=='created':
                self.assertIn('"phase": "created"',str(raised.exception))
                self.assertIn('"stage": "cells"',str(raised.exception))
                self.assertIn('exit=9',str(raised.exception))
            else:self.assertNotIn('"phase"',str(raised.exception))
    def test_foreign_provider_child_rejected(self):
        class App:
            bus_name=':9.9'
        root=Node('r');root.app=App()
        with self.assertRaises(RuntimeError):G.walk_tree(root,lambda f:f(),0,provider=':2.2')
    def test_actual_closed_envelope_rejects_bool_restoration(self):
        good=evidence('closed',True)
        H.validate_receipt(good,'closed',created=True)
        for field in ('focus','workspace','layout_id','layout_coordinate'):
            bad=copy.deepcopy(good)
            if field=='focus':bad['restored_focus']=True
            elif field=='workspace':bad['restored_workspace']=False
            else:
                # Independent copied restoration, so original remains strict integer data.
                bad['restored_layout']=copy.deepcopy(good['restored_layout'])
                if field=='layout_id':bad['restored_layout'][0][0]=True
                else:bad['restored_layout'][0][1][0]=False
            with self.subTest(field=field),self.assertRaises(RuntimeError):H.validate_receipt(bad,'closed',created=True)
    def test_host_minimal_success_refused_every_phase(self):
        for phase in H.PHASES:
            with self.subTest(phase=phase),self.assertRaises(RuntimeError):H.validate_receipt({'phase':phase,'ready':True},phase)
    def test_host_full_evidence_and_missing_known_assertions(self):
        for phase in H.PHASES:
            value=evidence(phase,phase in ('cancel-typed','cancelled','closed'))
            H.validate_receipt(value,phase,created=phase in ('cancel-typed','cancelled','closed'))
            for key in value:
                bad=copy.deepcopy(value);del bad[key]
                with self.subTest(phase=phase,key=key),self.assertRaises(RuntimeError):H.validate_receipt(bad,phase)
    def test_host_rejects_cancelled_and_wrong_provider(self):
        for mutation in ('cancelled_fs','cancelled_cell','wrong_provider','duplicate_cell','changed_window','wrong_scope','bool_geometry','wrong_seed'):
            value=evidence('cancelled',True)
            if mutation=='cancelled_fs':value['filesystem'][H.CANCELLED]=True
            elif mutation=='cancelled_cell':value['cancelled_cells']=1
            elif mutation=='wrong_provider':value['original']['accessibility']['provider']['pid']=99
            elif mutation=='duplicate_cell':value['created_cells']=2
            elif mutation=='changed_window':value['window_id']=7
            elif mutation=='wrong_scope':value['owner_scope']='authenticated per-surface owner'
            elif mutation=='bool_geometry':value['rect'][0]=True
            elif mutation=='wrong_seed':value['filesystem']['seed_sha256']='b'*64
            with self.subTest(mutation=mutation),self.assertRaises(RuntimeError):H.validate_receipt(value,'cancelled',window_id=2,created=True)
    def test_guest_cancellation_and_ambiguity_are_fatal(self):
        base={'seed_sha256':H.SEED_SHA,H.CREATED:True,H.CANCELLED:False}
        for files,cells in ((dict(base,**{H.CANCELLED:True}),{'seed.txt':1,H.CREATED:1,H.CANCELLED:0}),
                            (base,{'seed.txt':1,H.CREATED:1,H.CANCELLED:1}),
                            (base,{'seed.txt':2,H.CREATED:1,H.CANCELLED:0})):
            with self.assertRaises(RuntimeError):G.check_outcomes(files,cells)
    def test_provider_rejects_foreign_broker_pid_and_uid(self):
        original=original_evidence()['nautilus']
        def query(_conn,method,_name):return {"GetNameOwner":':2.2',"GetConnectionUnixProcessID":99,"GetConnectionUnixUser":1000}[method]
        with patch.object(G,'UID',1000),patch.object(G,'query_connection',side_effect=query),patch.object(G,'process',return_value=dict(original,pid=99)):
            with self.assertRaises(RuntimeError):G.provider_receipt(None,':2.2',original)
        with patch.object(G,'UID',1000),patch.object(G,'query_connection',side_effect=lambda c,m,n:':2.2' if m=='GetNameOwner' else 0 if m=='GetConnectionUnixUser' else 12),patch.object(G,'process',return_value={k:v for k,v in original.items() if k!='owner'}):
            with self.assertRaises(RuntimeError):G.provider_receipt(None,':2.2',original)
    def fixture(self,directory):
        root=Path(directory)/'fixture';root.mkdir(mode=0o700);seed=root/'seed.txt';seed.write_bytes(G.PAYLOAD);seed.chmod(0o600)
        folder=root/G.CREATED;folder.mkdir(mode=0o700);info=root.stat()
        return root,{'fixture':str(root),'fixture_dev':info.st_dev,'fixture_inode':info.st_ino,
            'seed_sha256':H.SEED_SHA,'seed_inode':[seed.stat().st_dev,seed.stat().st_ino],
            'created_inode':[folder.stat().st_dev,folder.stat().st_ino]}
    def test_cleanup_removes_only_original_known_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root,value=self.fixture(directory);receipt=G.cleanup_fixture(value)
            self.assertTrue(receipt['fixture_removed']);self.assertFalse(root.exists())
    def test_cleanup_refuses_extra_member_without_deleting_known_members(self):
        with tempfile.TemporaryDirectory() as directory:
            root,value=self.fixture(directory);(root/'unowned').write_bytes(b'not fixture data')
            with self.assertRaises(RuntimeError):G.cleanup_fixture(value)
            self.assertTrue((root/'seed.txt').exists());self.assertTrue((root/G.CREATED).exists())
    def test_cleanup_refuses_original_inode_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            root,value=self.fixture(directory);seed=root/'seed.txt';seed.rename(root/'original-seed-retained');seed.write_bytes(G.PAYLOAD)
            with self.assertRaises(RuntimeError):G.cleanup_fixture(value)
            self.assertTrue(seed.exists());self.assertTrue((root/G.CREATED).exists())
    def test_host_success_inputs_once_in_order(self):
        class Agent(AgentEvidence):pass
        class QMP:
            def __init__(self):self.inputs=[]
            def frame(self,_):pass
            def keys(self,*chord):self.inputs.append(('keys',chord))
            def type_text(self,value):self.inputs.append(('text',value))
        qmp=QMP();records=[]
        with tempfile.TemporaryDirectory() as out:H.run(qmp,Agent(),out,lambda *r:records.append(r))
        self.assertEqual(qmp.inputs,[('keys',('ctrl','shift','n')),('text',H.CREATED),('keys',('ret',)),('keys',('ctrl','shift','n')),('text',H.CANCELLED),('keys',('esc',)),('keys',('ctrl','w'))])
        self.assertTrue(records[-1][1])
    def test_host_failure_never_replays_input(self):
        class Agent(AgentEvidence):
            def run(self,action,phase,**kw):
                if phase=='dialog':raise RuntimeError('actual original admission failure')
                return super().run(action,phase,**kw)
        class QMP:
            def __init__(self):self.inputs=[]
            def frame(self,_):pass
            def keys(self,*chord):self.inputs.append(chord)
        qmp=QMP();records=[]
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaises(RuntimeError):H.run(qmp,Agent(),out,lambda *r:records.append(r))
        self.assertEqual(qmp.inputs,[('ctrl','shift','n')]);self.assertFalse(records[-1][1])

    def wrapper_failure(self,payload,code=9):
        # Compile the actual production main+validator, controlling only external
        # original inventory/runuser dependencies; no mirror wrapper implementation.
        tree=ast.parse((ROOT/'packaging/marlin/vm-lane/roost-vm-lifecycle').read_text())
        definitions=[node for node in tree.body if isinstance(node,ast.FunctionDef) and node.name in ('main','valid_native_files_error')]
        self.assertEqual(len(definitions),2)
        scope={'json':json,'sys':SimpleNamespace(argv=['fixed-lifecycle','native-files','mapped'],exit=lambda code:(_ for _ in ()).throw(SystemExit(code))),
               'os':SimpleNamespace(geteuid=lambda:0),'OWNER':SimpleNamespace(pw_uid=1000,pw_name='original-test-owner'),
               'RUNTIME':'/run/user/1000','inventory':lambda:{'processes':[{'executable':'roost-compositor','pid':34}],'session':{'Id':'1'}},
               'subprocess':SimpleNamespace(run=lambda *a,**k:SimpleNamespace(returncode=code,stdout=payload,stderr=b'private stderr must never survive'))}
        exec(compile(ast.Module(body=definitions,type_ignores=[]),'actual-lifecycle-wrapper','exec'),scope)
        output=io.StringIO()
        with redirect_stdout(output),self.assertRaises(SystemExit) as failure:scope['main']()
        self.assertEqual(failure.exception.code,code)
        self.assertNotIn('private',output.getvalue())
        return output.getvalue().encode()

    def test_actual_wrapper_retains_finite_helper_stage_and_nonzero(self):
        for stage in ('context','guard-route','window','tree','tree-provider','provider-enumerate','provider-receipt','tree-walk','cells','final-guard'):
            good={'phase':'mapped','stage':stage,'exception_type':'RuntimeError'}
            output=self.wrapper_failure(json.dumps({'native_files_error':good}).encode())
            self.assertEqual(json.loads(output),{'native_files_error':good})
            self.assertTrue(T.valid_native_files_error(good))

    def test_actual_wrapper_rejects_unknown_private_malformed_and_oversized(self):
        good={'phase':'mapped','stage':'tree','exception_type':'RuntimeError'}
        cases=[{'native_files_error':dict(good,message='private filename')},
               {'native_files_error':dict(good,phase='created')},
               {'native_files_error':dict(good,stage=[])},
               {'native_files_error':dict(good,exception_type={})},
               {'native_files_error':dict(good,stage='private unknown')},
               {'native_files_error':{k:v for k,v in good.items() if k!='stage'}},
               {'native_files_error':good,'body':'private body'}]
        payloads=[json.dumps(case).encode() for case in cases]+[b'private not JSON',b'x'*4097,b'['*1800+b']'*1800]
        for payload in payloads:
            with self.subTest(length=len(payload)):
                output=self.wrapper_failure(payload)
                self.assertEqual(json.loads(output),{'native_files_error':{'phase':'mapped','stage':'diagnostic-transport','exception_type':'RuntimeError'}})
                self.assertTrue(T.valid_native_files_error(json.loads(output)['native_files_error']))

    def test_actual_wrapper_guest_agent_host_failure_artifact_preserves_stage_exit(self):
        helper_output=io.StringIO()
        with patch.object(G.sys,'argv',['fixed-probe','mapped']),patch.object(G,'context',return_value={}), \
             patch.object(G,'guard',return_value={}),patch.object(G,'owned_window',side_effect=RuntimeError('private source error')),redirect_stdout(helper_output):
            self.assertEqual(G.main(),1)
        wrapped=self.wrapper_failure(helper_output.getvalue().encode(),code=9)
        agent=object.__new__(T.GuestAgent);current=[]
        def command(method,**arguments):
            if method=='guest-exec':current[:]=arguments['arg'];return {'pid':123}
            payload=json.dumps(evidence('start')).encode() if current==['native-files','start'] else wrapped
            return {'exited':True,'exitcode':0 if current==['native-files','start'] else 9,'out-data':base64.b64encode(payload).decode()}
        agent.command=command;records=[]
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaises(T.NativeFilesError) as error:
                H.run(SimpleNamespace(),agent,out,lambda *args:records.append(args),failure_receipt=T.native_files_failure)
            failure=json.loads((Path(out)/'native-files-pilot/failure.json').read_text())
            self.assertEqual(failure,{'exception_type':'NativeFilesError','native_files_error':{'phase':'mapped','stage':'window','exception_type':'RuntimeError'},'exitcode':9})
            self.assertEqual(error.exception.exitcode,9)
            self.assertEqual(len(json.loads((Path(out)/'native-files-pilot/observations.json').read_text())),1)
            self.assertNotIn('private',json.dumps(failure))
        self.assertFalse(records[-1][1])

    def test_typed_failure_rejects_forged_or_changed_fields(self):
        good={'phase':'mapped','stage':'window','exception_type':'RuntimeError'}
        for code in (True,0,-1,256,'9'):
            with self.assertRaises(ValueError):T.NativeFilesError(good,code)
        self.assertIsNone(T.native_files_failure(RuntimeError('private body')))
        error=T.NativeFilesError(good,9);error.diagnostic['message']='private'
        self.assertIsNone(T.native_files_failure(error))


class CellsQueryDiagnostic(unittest.TestCase):
    wrapper_failure=Policy.wrapper_failure
    def test_actual_fast_guard_finite_reason_before_after_and_rpc(self):
        value={'nautilus':{'pid':12,'start_ticks':2},'compositor':{'pid':34,'start_ticks':3},'display':{'original':True}}
        actual_path=G.Path
        class Proc:
            def __init__(self,name):self.name=name
            def stat(self):
                if boundary==G.QUERY_CONTEXT['boundary'] and reason=='process':
                    return SimpleNamespace(st_uid=G.UID+1)
                return SimpleNamespace(st_uid=G.UID)
            def resolve(self,strict=True):
                return actual_path('/usr/bin/nautilus' if '/12/' in self.name else '/usr/bin/roost-compositor')
        def path(name):
            return Proc(str(name)) if str(name).startswith('/proc/') else SimpleNamespace(resolve=lambda strict=True:actual_path(name))
        def bounded(path,*args,**kwargs):
            ticks=2 if '/12/' in path.name else 3
            return ('1 (original) '+' '.join(['S']+['0']*18+[str(ticks)])).encode()
        def route(value):
            return {'other':True} if boundary==G.QUERY_CONTEXT['boundary'] and reason=='route' else {'original':True}
        def scene():
            if boundary==G.QUERY_CONTEXT['boundary'] and reason=='scene':raise RuntimeError('private scene body')
            return {}
        for boundary in ('guard-before','guard-after'):
            for reason in ('process','route','scene','deadline'):
                with self.subTest(boundary=boundary,reason=reason),patch.object(G,'Path',side_effect=path), \
                     patch.object(G,'bounded',side_effect=bounded),patch.object(G,'display_receipt',side_effect=route), \
                     patch.object(G,'scene',side_effect=scene),patch.object(G,'deadline',side_effect=lambda: (_ for _ in ()).throw(TimeoutError('private clock')) if boundary==G.QUERY_CONTEXT['boundary'] and reason=='deadline' else None):
                    G.error_stage('cells');G.QUERY_CONTEXT=None;called=[]
                    with self.assertRaises((RuntimeError,TimeoutError)):
                        G.Queries(value).call(lambda:called.append(True),kind='role')
                    self.assertEqual(G.QUERY_CONTEXT,{'operation':'role','boundary':boundary,'reason':reason})
                    self.assertEqual(called,[] if boundary=='guard-before' else [True])
        for kind in ('role','name','state'):
            error=RuntimeError('private RPC body')
            with patch.object(G,'fast_guard',return_value={}):
                G.error_stage('cells')
                with self.assertRaises(RuntimeError) as failed:
                    G.Queries(value).call(lambda:(_ for _ in ()).throw(error),kind=kind)
                self.assertIs(failed.exception,error)
                self.assertEqual(G.QUERY_CONTEXT,{'operation':kind,'boundary':'operation','reason':'rpc'})
                self.assertEqual(G.Queries(value).call(lambda:7,kind=kind),7)
                self.assertIsNone(G.QUERY_CONTEXT)

    def test_actual_helper_wrapper_agent_host_nested_context_roundtrip(self):
        original=RuntimeError('private node name or RPC body')
        class Node:
            def getRoleName(self):raise original
        output=io.StringIO()
        with patch.object(G.sys,'argv',['fixed-probe','mapped']),patch.object(G,'context',return_value={}), \
             patch.object(G,'guard',return_value={}),patch.object(G,'owned_window',return_value={}), \
             patch.object(G,'accessibility',return_value=([Node()],SimpleNamespace(),G.Queries({}).call,1)), \
             patch.object(G,'fast_guard',return_value={}),redirect_stdout(output):
            self.assertEqual(G.main(),1)
        good={'phase':'mapped','stage':'cells','exception_type':'RuntimeError',
              'query_context':{'operation':'role','boundary':'operation','reason':'rpc'}}
        self.assertEqual(json.loads(output.getvalue()),{'native_files_error':good})
        wrapped=self.wrapper_failure(output.getvalue().encode(),code=9)
        agent=object.__new__(T.GuestAgent);current=[]
        def command(method,**arguments):
            if method=='guest-exec':current[:]=arguments['arg'];return {'pid':123}
            payload=json.dumps(evidence('start')).encode() if current==['native-files','start'] else wrapped
            return {'exited':True,'exitcode':0 if current==['native-files','start'] else 9,'out-data':base64.b64encode(payload).decode()}
        agent.command=command
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaises(T.NativeFilesError) as failed:
                H.run(SimpleNamespace(),agent,out,lambda *args:None,failure_receipt=T.native_files_failure)
            receipt=json.loads((Path(out)/'native-files-pilot/failure.json').read_text())
            self.assertEqual(receipt,{'exception_type':'NativeFilesError','native_files_error':good,'exitcode':9})
            self.assertEqual(failed.exception.exitcode,9)
            self.assertNotIn('private',json.dumps(receipt))

    def test_every_finite_nested_context_and_unknown_fields_refused_through_wrapper(self):
        good={'phase':'mapped','stage':'cells','exception_type':'RuntimeError'}
        for operation in ('role','name','state'):
            for boundary,reasons in (('operation',('rpc',)),('guard-before',('process','route','scene','deadline')),('guard-after',('process','route','scene','deadline'))):
                for reason in reasons:
                    candidate=dict(good,query_context={'operation':operation,'boundary':boundary,'reason':reason})
                    self.assertTrue(T.valid_native_files_error(candidate))
                    self.assertEqual(json.loads(self.wrapper_failure(json.dumps({'native_files_error':candidate}).encode())),{'native_files_error':candidate})
        query={'operation':'role','boundary':'operation','reason':'rpc'}
        bad=[None,[],dict(query,message='private'),dict(query,operation='private'),dict(query,operation=True),
             dict(query,boundary='private'),dict(query,reason='scene'),dict(query,boundary='guard-before',reason='rpc'),
             {k:v for k,v in query.items() if k!='reason'}]
        for context in bad:
            candidate=dict(good,query_context=context)
            self.assertFalse(T.valid_native_files_error(candidate))
            result=json.loads(self.wrapper_failure(json.dumps({'native_files_error':candidate}).encode()))
            self.assertEqual(result,{'native_files_error':{'phase':'mapped','stage':'diagnostic-transport','exception_type':'RuntimeError'}})
        self.assertFalse(T.valid_native_files_error(dict(good,stage='tree',query_context=query)))

if __name__=='__main__':unittest.main()
