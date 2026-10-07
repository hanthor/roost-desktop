#!/usr/bin/python3
"""Pure production-path policy tests; no actual GTK/native qualification."""
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import copy
import base64
import os
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
        good={'phase':'created','exception_type':'RuntimeError'}
        self.assertTrue(T.valid_native_files_error(good))
        for candidate in (dict(good,message='private content'),dict(good,phase=[]),dict(good,exception_type='private body')):
            self.assertFalse(T.valid_native_files_error(candidate))
    def test_original_files_route_not_private_env(self):
        value={'nautilus':{'pid':12},'compositor':{'pid':34}}
        with patch.object(G,'bounded',return_value=b'PRIVATE=never retained\0WAYLAND_DISPLAY=roost-nested-34\0'):
            G.original_app_route(value)
        with patch.object(G,'bounded',return_value=b'WAYLAND_DISPLAY=parent-host\0'):
            with self.assertRaises(RuntimeError):G.original_app_route(value)
    def test_transport_diagnostic_matches_requested_phase(self):
        for phase in ('mapped','created'):
            agent=object.__new__(T.GuestAgent)
            status={'exited':True,'exitcode':9,'out-data':base64.b64encode(__import__('json').dumps({'native_files_error':{'phase':phase,'exception_type':'RuntimeError'}}).encode()).decode()}
            agent.command=lambda method,**kw:{'pid':123} if method=='guest-exec' else status
            with self.assertRaises(RuntimeError) as raised:agent.run('native-files','created')
            if phase=='created':self.assertIn('"phase": "created"',str(raised.exception))
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
if __name__=='__main__':unittest.main()
