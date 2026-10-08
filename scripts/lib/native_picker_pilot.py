"""Native ordinary-host GTK FileDialog proof; independently authored QMP input."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import re
import time

_loader=importlib.machinery.SourceFileLoader('picker_files_policy',str(Path(__file__).with_name('native_files_pilot.py')))
_spec=importlib.util.spec_from_loader(_loader.name,_loader)
files=importlib.util.module_from_spec(_spec);_loader.exec_module(files)
PHASES={'start','parent','parent-clicked','parent-tested','grant-dialog','blocked','selected','granted','cancel-dialog','dismissed','restored-clicked','restored-tested','closed'}
APP='org.example.RoostNativeFileDialog'
TITLE='Roost native FileDialog proof'
SEED='known seed %25.txt'
PAYLOAD=b'Roost native GTK FileDialog selected ordinary host file\n'
SHA=hashlib.sha256(PAYLOAD).hexdigest()
SCOPE='ordinary host OpenFile; authenticated process/provider; surface identity correlation only'
PARENT_PHASES={'parent','parent-clicked','parent-tested','granted','dismissed','restored-clicked','restored-tested'}


def expected_events(phase):
    if type(phase) is not str or phase not in PHASES or phase=='start':raise RuntimeError('fixed actual event phase required')
    calls=0 if phase in ('parent','parent-clicked','parent-tested') else 2 if phase in ('cancel-dialog','dismissed','restored-clicked','restored-tested','closed') else 1
    callbacks=2 if phase in ('dismissed','restored-clicked','restored-tested','closed') else 1 if phase in ('granted','cancel-dialog') else 0
    clicks=0 if phase=='parent' else 2 if phase in ('restored-clicked','restored-tested','closed') else 1
    keys=0 if phase in ('parent','parent-clicked') else 2 if phase in ('restored-tested','closed') else 1
    return {'open_count':calls,'portal_calls':calls,'portal_responses':callbacks,'callback_count':callbacks,'parent_clicks':clicks,'parent_keys':keys}


def request_fields(parent,title,options,directory,chooser_title):
    token=options.get('handle_token') if type(options) is dict else None
    folder=options.get('current_folder') if type(options) is dict else None
    if (title!=chooser_title or type(parent) is not str or not parent.startswith('wayland:') or not 9<=len(parent)<=512
            or type(token) is not str or re.fullmatch('[A-Za-z0-9_]{1,128}',token) is None
            or options.get('modal') is not True or options.get('multiple',False) is not False or options.get('directory',False) is not False or type(folder) not in (bytes,list,tuple)
            or len(folder)>4096 or any(type(v) is not int or not 0<=v<=255 for v in folder)
            or bytes(folder)!=os.fsencode(directory)+b'\0'):
        raise RuntimeError('actual GTK exported modal/controlled folder request')
    return token


def response_fields(code,result,expected_uri):
    if type(code) is not int or code not in (0,1) or type(result) is not dict:raise RuntimeError('actual portal response type')
    uris=result.get('uris',[])
    if type(uris) is not list or (uris!=[expected_uri] if code==0 else uris!=[]):raise RuntimeError('actual known grant/dismiss response')


def package(value):
    names={'gtk4','nautilus','xdg-desktop-portal','xdg-desktop-portal-gnome','python-gobject','python-atspi','roost'}
    if type(value) is not dict or set(value)!=names or any(type(v) is not str or re.fullmatch(re.escape(k)+r' [0-9][A-Za-z0-9.+_:~-]{0,95}',v) is None for k,v in value.items()):raise RuntimeError('native picker actual package schema')


def original(value):
    keys={'base','frontend','backend','route_sha256','metadata_sha256','packages','fixture','seed_sha256','caller_script_sha256'}
    if type(value) is not dict or set(value)!=keys:raise RuntimeError('native picker original schema')
    files.validate_original(value['base']);uid=value['base']['nautilus']['uid']
    files.principal(value['frontend'],uid);files.principal(value['backend'],uid)
    package(value['packages'])
    for key in ('route_sha256','metadata_sha256','caller_script_sha256'):
        if type(value[key]) is not str or re.fullmatch('[0-9a-f]{64}',value[key]) is None:raise RuntimeError('native picker original digest schema')
    if value['seed_sha256']!=SHA or type(value['fixture']) is not str or re.fullmatch(r'/run/user/'+str(uid)+r'/roost-native-picker-[A-Za-z0-9_]{8}',value['fixture']) is None:raise RuntimeError('native picker controlled seed schema')


def rect(value):
    if type(value) is not list or len(value)!=4 or any(type(v) is not int for v in value) or not 1<=value[2]<=1280 or not 1<=value[3]<=800 or not 0<=value[0]<=1280-value[2] or not 0<=value[1]<=800-value[3]:raise RuntimeError('native picker rectangle schema')


def validate(value,phase,initial=None,caller=None,parent=None):
    common={'phase','ready','scope','original','principals_retained','filesystem_unchanged'}
    if type(value) is not dict or type(phase) is not str or value.get('phase')!=phase or phase not in PHASES or type(value.get('ready')) is not bool or value.get('scope')!=SCOPE or value.get('principals_retained') is not True or value.get('filesystem_unchanged') is not True:raise RuntimeError('native picker evidence schema')
    original(value.get('original'))
    if initial is not None and value['original']!=initial:raise RuntimeError('native picker original changed')
    if phase=='start':
        if not value['ready'] or set(value)!=common:raise RuntimeError('native picker start evidence')
        return
    keys=common|{'caller','caller_provider','parent_id','parent_rect','ui_sequence','open_count','parent_clicks','parent_keys','portal_calls','portal_responses','callback_count','granted','dismissed','uri_matches','read_sha256'}
    if set(value)!=keys|({'dialog_id','dialog_rect','target','selected','nautilus_provider'} if phase in ('grant-dialog','blocked','selected','cancel-dialog') else {'open_target','probe_target'} if phase in PARENT_PHASES else {'restored_layout','restored_focus','restored_workspace','cleanup'} if phase=='closed' else set()):raise RuntimeError('native picker finite phase fields')
    files.principal(value['caller'],value['original']['base']['nautilus']['uid'],owner=False)
    files.principal(value['caller_provider'],value['caller']['uid'])
    if any(value['caller_provider'][key]!=value['caller'][key] for key in value['caller']):raise RuntimeError('native picker caller provider mismatch')
    if caller is not None and value['caller']!=caller:raise RuntimeError('native picker caller replaced')
    if not files.integer(value['parent_id'],1) or value['parent_id'] in value['original']['base']['original_windows'] or (parent is not None and parent!=value['parent_id']):raise RuntimeError('native picker parent correlation')
    rect(value['parent_rect'])
    for key,limit in (('ui_sequence',2**63-1),('open_count',2),('portal_calls',2),('portal_responses',2),('callback_count',2),('parent_clicks',2),('parent_keys',2)):
        if not files.integer(value[key],0,limit):raise RuntimeError('native picker actual event counts')
    for key in ('granted','dismissed','uri_matches'):
        if type(value[key]) is not bool:raise RuntimeError('native picker callback type')
    grant=phase in ('granted','cancel-dialog','dismissed','restored-clicked','restored-tested','closed')
    dismiss=phase in ('dismissed','restored-clicked','restored-tested','closed')
    if value['ready']:
        if any(value[key]!=count for key,count in expected_events(phase).items()) or value['granted']!=grant or value['dismissed']!=dismiss or value['uri_matches']!=grant or value['read_sha256']!=(SHA if grant else None):raise RuntimeError('native picker real portal/callback/input phase')
    elif phase in ('parent','closed') or value['read_sha256'] not in (None,SHA):raise RuntimeError('native picker pending schema')
    if phase in PARENT_PHASES:
        rect(value['open_target']);rect(value['probe_target'])
    if phase in ('grant-dialog','blocked','selected','cancel-dialog'):
        rect(value['dialog_rect']);rect(value['target'])
        if not files.integer(value['dialog_id'],1) or value['dialog_id']==value['parent_id'] or value['dialog_id'] in value['original']['base']['original_windows'] or type(value['selected']) is not bool or (value['ready'] and phase=='selected' and not value['selected']):raise RuntimeError('native picker real dialog correlation')
        files.principal(value['nautilus_provider'],value['caller']['uid'])
        if value['nautilus_provider']!=value['original']['base']['accessibility']['provider']:raise RuntimeError('native picker original Nautilus provider changed')
    if phase=='closed':
        base=value['original']['base']
        files.validate_original(dict(base,original_layout=value['restored_layout'],original_focus=value['restored_focus'],original_workspace=value['restored_workspace']))
        if value['restored_layout']!=base['original_layout'] or value['restored_focus']!=base['original_focus'] or value['restored_workspace']!=base['original_workspace'] or value['cleanup'] is not True:raise RuntimeError('native picker original restoration')


def run(qmp,agent,out,record):
    output=Path(out)/'native-gtk-picker';output.mkdir(exist_ok=True)
    sequence=[];initial=None;caller=None;parent=None
    def observe(phase,wait=True,min_sequence=None):
        nonlocal initial,caller,parent
        end=time.monotonic()+45
        while True:
            value=agent.run('native-picker',phase,timeout=30)
            validate(value,phase,initial,caller,parent)
            if len(sequence)>=256:raise RuntimeError('native picker receipt bound')
            sequence.append(value);(output/'observations.json').write_text(json.dumps(sequence,indent=2))
            if value['ready'] and (min_sequence is None or value['ui_sequence']>min_sequence):
                if phase=='start':initial=value['original']
                elif caller is None:caller=value['caller'];parent=value['parent_id']
                return value
            if not wait or time.monotonic()>=end:raise TimeoutError('native picker readiness expired')
            time.sleep(.2)
    def click(phase,field):
        value=observe(phase,False);x,y,w,h=value[field];qmp.click(x+w//2,y+h//2);return value['ui_sequence']
    try:
        observe('start',False);observe('parent');qmp.frame(str(output/'parent.png'))
        before=click('parent','probe_target');observe('parent-clicked',min_sequence=before)
        value=observe('parent-clicked',False);qmp.keys('f8');observe('parent-tested',min_sequence=value['ui_sequence'])
        click('parent-tested','open_target');observe('grant-dialog');qmp.frame(str(output/'grant-dialog.png'))
        # Probe the measured genuine parent button only if wholly outside its modal child.
        # Its unchanged counters plus advancing caller heartbeat demonstrate input exclusion.
        value=observe('blocked',False);x,y,w,h=value['target'];qmp.click(x+w//2,y+h//2)
        observe('blocked',min_sequence=value['ui_sequence'])
        value=observe('blocked',False);qmp.keys('f8');observe('blocked',min_sequence=value['ui_sequence'])
        click('grant-dialog','target');observe('selected')
        observe('selected',False);qmp.keys('ret');observe('granted');qmp.frame(str(output/'granted.png'))
        click('granted','open_target');observe('cancel-dialog')
        observe('cancel-dialog',False);qmp.keys('esc');observe('dismissed');qmp.frame(str(output/'dismissed.png'))
        before=click('dismissed','probe_target');observe('restored-clicked',min_sequence=before)
        value=observe('restored-clicked',False);qmp.keys('f8');observe('restored-tested',min_sequence=value['ui_sequence'])
        observe('restored-tested',False);qmp.keys('ctrl','w');observe('closed');qmp.frame(str(output/'restored.png'))
    except Exception as error:
        (output/'failure.json').write_text(json.dumps({'exception_type':type(error).__name__}))
        record('V-GTK-NATIVE-PICKER',False,'native Gtk.FileDialog grant/user-dismiss original-session journey failed');raise
    record('V-GTK-NATIVE-PICKER',True,'actual ordinary host Gtk.FileDialog OpenFile grant/read/user-dismiss; original principals and layout restored')
