"""Independently authored real native Files/QMP journey, never a UI setter."""
import json
import hashlib
import re
import stat
import os
import time

CREATED='created-through-files'
CANCELLED='cancelled-through-files'
PHASES={'start','mapped','dialog','create-typed','created','cancel-typed','cancelled','closed'}


OWNER_SCOPE='original process/provider authenticated; compositor window ID/app-id/geometry correlation only'
SEED_SHA=hashlib.sha256(b'Roost native Files pilot known seed\n').hexdigest()


def integer(value,low=0,high=2**64-1):return type(value) is int and low<=value<=high


def principal(value,uid=None,owner=True):
    keys={'pid','uid','start_ticks','exe_sha256'}|({'owner'} if owner else set())
    if type(value) is not dict or set(value)!=keys or not all(integer(value[k],1) for k in ('pid','uid','start_ticks')) or not integer(value['pid'],2,2**32-1):raise RuntimeError('native Files principal schema')
    if uid is not None and value['uid']!=uid:raise RuntimeError('native Files principal UID mismatch')
    if type(value['exe_sha256']) is not str or re.fullmatch('[0-9a-f]{64}',value['exe_sha256']) is None:raise RuntimeError('native Files executable receipt schema')
    if owner and (type(value['owner']) is not str or re.fullmatch(r':[0-9]{1,10}\.[0-9]{1,10}',value['owner']) is None):raise RuntimeError('native Files owner schema')


def validate_original(value):
    keys={'nautilus','compositor','accessibility','session','original_windows','original_layout','original_focus','original_workspace'}
    if type(value) is not dict or set(value)!=keys:raise RuntimeError('native Files original receipt schema')
    principal(value['nautilus']);uid=value['nautilus']['uid'];principal(value['compositor'],uid)
    a11y=value['accessibility']
    if type(a11y) is not dict or set(a11y)!={'socket','broker','provider','transport_peer'}:raise RuntimeError('native Files accessibility receipt schema')
    principal(a11y['provider'],uid);principal(a11y['broker'],uid,owner=False);principal(a11y['transport_peer'],uid,owner=False)
    if any(a11y['provider'][k]!=value['nautilus'][k] for k in ('pid','uid','start_ticks','exe_sha256')):raise RuntimeError('native Files provider principal mismatch')
    sock=a11y['socket']
    if type(sock) is not dict or set(sock)!={'dev','inode','uid','mode'} or not all(integer(v) for v in sock.values()) or sock['uid']!=uid or not stat.S_ISSOCK(sock['mode']):raise RuntimeError('native Files broker socket schema')
    session=value['session']
    if type(session) is not dict or set(session)!={'Id','VTNr','User','Service','Class'} or type(session['Id']) is not str or re.fullmatch('[A-Za-z0-9]{1,32}',session['Id']) is None or session['VTNr']!='1' or session['User']!=str(uid) or session['Service']!='greetd' or session['Class'] not in ('user','user-early'):raise RuntimeError('native Files login session schema')
    ids=value['original_windows'];layout=value['original_layout']
    if type(ids) is not list or len(ids)>64 or any(not integer(v,1) for v in ids) or ids!=sorted(set(ids)):raise RuntimeError('native Files original window schema')
    if type(layout) is not list or len(layout)!=len(ids):raise RuntimeError('native Files original layout schema')
    for identifier,row in zip(ids,layout):
        if type(row) is not list or len(row)!=2 or type(row[0]) is not int or row[0]!=identifier or type(row[1]) is not list or len(row[1])!=4 or any(not integer(v,-2**31,2**31-1) for v in row[1]) or row[1][2]<=0 or row[1][3]<=0:raise RuntimeError('native Files original layout member schema')
    if value['original_focus'] is not None and (type(value['original_focus']) is not int or value['original_focus'] not in ids):raise RuntimeError('native Files original focus schema')
    if not integer(value['original_workspace'],0,4096):raise RuntimeError('native Files original workspace schema')


def validate_receipt(value,phase,original=None,window_id=None,created=False):
    common={'phase','ready','original','owner_scope','original_principals_retained','filesystem'}
    if type(value) is not dict or value.get('phase')!=phase or type(value.get('ready')) is not bool or value.get('owner_scope')!=OWNER_SCOPE or value.get('original_principals_retained') is not True:raise RuntimeError('native Files evidence admission schema')
    validate_original(value.get('original'))
    if original is not None and value['original']!=original:raise RuntimeError('native Files original principals changed')
    files=value.get('filesystem')
    if type(files) is not dict or set(files)!={'seed_sha256',CREATED,CANCELLED} or files['seed_sha256']!=SEED_SHA or type(files[CREATED]) is not bool or files[CANCELLED] is not False:raise RuntimeError('native Files seed/cancel filesystem evidence')
    expected_created=created or phase in ('created','cancelled','closed')
    if value['ready'] and files[CREATED]!=expected_created:raise RuntimeError('native Files created-directory evidence')
    if not value['ready']:
        if set(value)!=common or phase=='start' or ((phase!='created' or created) and files[CREATED]!=expected_created):raise RuntimeError('native Files pending evidence schema')
        return
    if phase=='start':
        if set(value)!=common|{'packages'}:raise RuntimeError('native Files start evidence schema')
        packages=value['packages']
        if type(packages) is not dict or set(packages)!={'nautilus','python-atspi','roost'} or any(type(v) is not str or re.fullmatch(re.escape(k)+r' [0-9][A-Za-z0-9.+_:~-]{0,95}',v) is None for k,v in packages.items()):raise RuntimeError('native Files genuine package receipt schema')
        return
    if phase=='closed':
        if set(value)!=common|{'restored','restored_layout','restored_focus','restored_workspace','cleanup'} or value['restored'] is not True or value['restored_layout']!=value['original']['original_layout'] or value['restored_focus']!=value['original']['original_focus'] or value['restored_workspace']!=value['original']['original_workspace']:raise RuntimeError('native Files original restoration evidence')
        restoration=dict(value['original'],original_layout=value['restored_layout'],original_focus=value['restored_focus'],original_workspace=value['restored_workspace'])
        validate_original(restoration)
        if value['cleanup']!={'fixture_inode_matched':True,'known_members_removed':True,'fixture_removed':True,'context_retained':True} or any(type(v) is not bool for v in value['cleanup'].values()):raise RuntimeError('native Files cleanup evidence schema')
        return
    keys=common|{'window_id','rect','node_count','showing_node_count','seed_cells','created_cells','cancelled_cells','dialog_count','focused_entry_count'}
    if phase in ('create-typed','cancel-typed'):keys|={'known_entry_text_matches'}
    if set(value)!=keys or not integer(value['window_id'],1) or value['window_id'] in value['original']['original_windows'] or (window_id is not None and value['window_id']!=window_id):raise RuntimeError('native Files window correlation evidence')
    rect=value['rect']
    if type(rect) is not list or len(rect)!=4 or any(type(v) is not int for v in rect) or not 100<=rect[2]<=1280 or not 100<=rect[3]<=800 or not 0<=rect[0]<=1280-rect[2] or not 0<=rect[1]<=800-rect[3]:raise RuntimeError('native Files correlated window geometry schema')
    if not integer(value['node_count'],1,4096) or not integer(value['showing_node_count'],1,value['node_count']) or any(not integer(value[k],0,1) for k in ('seed_cells','created_cells','cancelled_cells','dialog_count','focused_entry_count')) or value['cancelled_cells']!=0:raise RuntimeError('native Files bounded known accessibility evidence')
    if phase in ('dialog','create-typed','cancel-typed'):
        if value['dialog_count']!=1 or value['focused_entry_count']!=1:raise RuntimeError('native Files actual original dialog focus evidence')
    elif value['dialog_count']!=0:raise RuntimeError('native Files dialog did not close')
    if phase=='mapped' and value['seed_cells']!=1:raise RuntimeError('native Files seed cell missing')
    if phase in ('created','cancelled') and value['created_cells']!=1:raise RuntimeError('native Files created cell missing')
    if phase in ('create-typed','cancel-typed') and value['known_entry_text_matches'] is not True:raise RuntimeError('native Files known actual input evidence')


def run(qmp,agent,out,record,failure_receipt=None):
    evidence=os.path.join(out,'native-files-pilot');os.makedirs(evidence,exist_ok=True)
    sequence=[];original=None;window_id=None;created=False
    def observe(phase,wait=True):
        nonlocal original,window_id,created
        # 45s retry window plus at most one final bounded 30s guest query.
        end=time.monotonic()+45
        while True:
            value=agent.run('native-files',phase,timeout=30)
            validate_receipt(value,phase,original,window_id,created)
            if value['ready']:
                if phase=='start':original=value['original']
                elif phase!='closed':window_id=value['window_id']
                if phase=='created':created=True
            if len(sequence)>=256:raise RuntimeError("native Files receipt count bound")
            sequence.append(value)
            with open(os.path.join(evidence,'observations.json'),'w') as stream:json.dump(sequence,stream,indent=2)
            if value['ready']:return value
            if not wait or time.monotonic()>=end:raise RuntimeError('native Files original readiness deadline expired')
            time.sleep(.2)
    def picture(label):qmp.frame(os.path.join(evidence,label+'.png'))
    def keys(phase,*chord):
        # A fresh complete original-session/window admission BEFORE each balanced input.
        observe(phase,wait=False)
        qmp.keys(*chord)
    try:
        observe('start',wait=False)
        observe('mapped');picture('mapped')
        keys('mapped','ctrl','shift','n');observe('dialog')
        observe('dialog',wait=False);qmp.type_text(CREATED)
        keys('create-typed','ret');observe('created');picture('created')
        keys('created','ctrl','shift','n');observe('dialog')
        observe('dialog',wait=False);qmp.type_text(CANCELLED)
        keys('cancel-typed','esc');observe('cancelled');picture('cancelled')
        keys('cancelled','ctrl','w');observe('closed');picture('restored')
    except Exception as error:
        # Error originates solely in fixed transport/schema paths; redact arbitrary bodies.
        failure={"exception_type":type(error).__name__}
        if failure_receipt is not None:
            finite=failure_receipt(error)
            if finite is not None:failure.update(finite)
        with open(os.path.join(evidence,"failure.json"),"w") as stream:json.dump(failure,stream)
        record('V-NAUTILUS-NATIVE-PILOT',False,'native Files create/cancel original-owner journey failed')
        raise
    record('V-NAUTILUS-NATIVE-PILOT',True,'actual native Files new-folder/cancel; original process/provider and seed hash retained; correlated window/layout restored')
