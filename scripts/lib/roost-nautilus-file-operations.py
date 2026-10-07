#!/usr/bin/python3
"""Exercise genuine Files operations through ordinary desktop input in CI."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

import pyatspi
from gi.repository import Gio, GLib

OUT = Path('/out')
STATE = OUT / 'compositor-state.json'
records = []
stage = 'launch'
identity = None
fixture = None


def run(*arguments):
    return subprocess.check_output(arguments, text=True, timeout=15)


def scene():
    result = json.loads(STATE.read_text())
    if result.get('locked'):
        raise RuntimeError('Files operation attempted while the desktop is locked')
    return result


def nautilus_identity():
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

    def call(method, signature, name):
        return bus.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                             'org.freedesktop.DBus', method,
                             GLib.Variant('(s)', (name,)), GLib.VariantType.new(signature),
                             Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]

    owner = call('GetNameOwner', '(s)', 'org.gnome.Nautilus')
    pid = call('GetConnectionUnixProcessID', '(u)', owner)
    uid = call('GetConnectionUnixUser', '(u)', owner)
    executable = Path(f'/proc/{pid}/exe').resolve(strict=True)
    installed = Path('/usr/bin/nautilus').resolve(strict=True)
    metadata = installed.stat()
    if uid != os.getuid() or executable != installed or metadata.st_uid != 0 or metadata.st_mode & 0o022:
        raise RuntimeError('Files owner is not the genuine installed Nautilus')
    return {'owner': owner, 'pid': pid, 'uid': uid, 'exe': str(executable)}


def controls():
    while GLib.MainContext.default().pending():
        GLib.MainContext.default().iteration(False)
    result = []

    def walk(node):
        try:
            node.clear_cache()
            if node.getState().contains(pyatspi.STATE_SHOWING):
                result.append(node)
            for index in range(node.childCount):
                walk(node.getChildAtIndex(index))
        except Exception:
            pass

    desktop = pyatspi.Registry.getDesktop(0)
    for index in range(desktop.childCount):
        app = desktop.getChildAtIndex(index)
        try:
            app_pid = app.get_process_id()
        except Exception:
            # Earlier picker callers may disappear between accessibility reads.
            continue
        if identity is not None and app_pid == identity['pid']:
            walk(app)
    return result


def snapshot(label):
    nodes = [{'name': node.name, 'role': node.getRoleName(),
              'selected': node.getState().contains(pyatspi.STATE_SELECTED),
              'focused': node.getState().contains(pyatspi.STATE_FOCUSED),
              'text': node.queryText().getText(0, -1)
              if node.getRoleName() in ('text', 'entry') else None}
             for node in controls()]
    (OUT / ('nautilus-' + label + '.json')).write_text(
        json.dumps({'scene': scene(), 'a11y': nodes}, indent=2))
    subprocess.run(['scrot', str(OUT / ('nautilus-' + label + '.png'))], check=True)


def wait(predicate, description):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.05)
    raise RuntimeError('Files did not ' + description)


def focused():
    current = scene()
    if nautilus_identity() != identity:
        raise RuntimeError('Files original bus owner or installed process changed')
    if current.get('focused_app_id') != 'org.gnome.Nautilus':
        raise RuntimeError('Files lost actual compositor keyboard focus')
    return current


def key(value):
    focused()
    run('xdotool', 'key', '--clearmodifiers', value)


def type_text(value):
    focused()
    run('xdotool', 'type', '--clearmodifiers', '--delay', '0', '--', value)


def cell(name):
    cells = [node for node in controls() if node.getRoleName() == 'table cell'
             and node.name in (name, name + '. File', name + '. Folder')]
    if len(cells) != 1:
        return None
    return cells[0]


def select(name):
    wait(lambda: cell(name) is not None, 'show the actual file cell ' + name)
    node = cell(name)
    bounds = node.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
    current = focused()
    fx, fy, fw, fh = current['focused_rect']
    if (bounds.width <= 0 or bounds.height <= 0 or bounds.x < 0 or bounds.y < 0
            or bounds.x + bounds.width > fw or bounds.y + bounds.height > fh):
        raise RuntimeError('Files cell is outside its actual focused surface')
    run('xdotool', 'mousemove', '--window', host,
        str(fx + bounds.x + bounds.width // 2), str(fy + bounds.y + bounds.height // 2), 'click', '1')
    wait(lambda: cell(name) is not None and cell(name).getState().contains(pyatspi.STATE_SELECTED),
         'select the actual file ' + name)


def navigate(path):
    if not path.is_absolute() or not path.is_dir():
        raise RuntimeError('Files navigation requires an actual absolute fixture directory')
    key('ctrl+l')
    # Nautilus 51 completes directory names with a separator. Type the
    # canonical directory spelling so the exact focused-entry assertion
    # remains valid whether its asynchronous completion has run or not.
    rename_entry(str(path) + '/')


def rename_entry(value):
    wait(lambda: any(node.getRoleName() in ('text', 'entry') and
                    node.getState().contains(pyatspi.STATE_FOCUSED) for node in controls()),
         'focus an actual editable name control')
    key('ctrl+a')
    type_text(value)
    wait(lambda: any(node.getRoleName() in ('text', 'entry') and
                    node.getState().contains(pyatspi.STATE_FOCUSED) and
                    node.queryText().getText(0, -1) == value for node in controls()),
         'show the exact name in its focused edit control')
    key('Return')


def has_payload(path, payload):
    return path.is_file() and path.read_bytes() == payload


def trash_matches(original, payload):
    trash = Gio.File.new_for_uri('trash:///')
    iterator = trash.enumerate_children('standard::name,trash::orig-path',
                                       Gio.FileQueryInfoFlags.NONE, None)
    result = []
    try:
        while True:
            info = iterator.next_file(None)
            if info is None:
                break
            if info.get_attribute_byte_string('trash::orig-path') == str(original):
                child = trash.get_child(info.get_name())
                ok, contents, unused = child.load_contents(None)
                if not ok or bytes(contents) != payload:
                    raise RuntimeError('Actual Trash item lost the selected file contents')
                result.append(child.get_uri())
    finally:
        iterator.close(None)
    return result


try:
    if os.getuid() != 1000:
        raise RuntimeError('Files proof requires the ordinary session user')
    windows = run('xdotool', 'search', '--onlyvisible', '--name', '^Smithay').split()
    if len(windows) != 1:
        raise RuntimeError('Expected one actual nested desktop host')
    host = windows[0]
    run('xdotool', 'windowfocus', host)
    # Container-local fixture remains available when an assertion fails.
    fixture = Path(tempfile.mkdtemp(prefix='roost-files-', dir=Path.home()))
    name = 'seed-' + uuid.uuid4().hex + '.txt'
    seed = fixture / name
    payload = b'Roost GNOME Files actual operation roundtrip\n' + name.encode() + b'\n'
    seed.write_bytes(payload)
    run('nautilus', '--new-window', str(fixture))
    identity = nautilus_identity()
    wait(lambda: scene().get('focused_app_id') == 'org.gnome.Nautilus' and cell(name) is not None,
         'map its native window with the actual fixture contents')
    browser_id = scene()['focused']
    snapshot('opened')
    stage = 'create-folder'
    key('ctrl+shift+n')
    folder_name = 'created-through-files'
    rename_entry(folder_name)
    folder = fixture / folder_name
    wait(lambda: folder.is_dir(), 'create the folder through its real dialog')
    snapshot('folder-created')
    stage = 'copy'
    select(name)
    key('ctrl+c')
    navigate(folder)
    wait(lambda: cell(name) is None, 'navigate into the empty destination folder')
    key('ctrl+v')
    copied = folder / name
    wait(lambda: has_payload(copied, payload) and has_payload(seed, payload) and cell(name) is not None,
         'copy the selected file through the real clipboard')
    snapshot('copied')
    stage = 'rename'
    select(name)
    key('F2')
    renamed_name = 'renamed-through-files.txt'
    rename_entry(renamed_name)
    renamed = folder / renamed_name
    wait(lambda: not copied.exists() and has_payload(renamed, payload) and cell(renamed_name) is not None,
         'rename the actual selected file')
    snapshot('renamed')
    stage = 'move'
    select(renamed_name)
    key('ctrl+x')
    navigate(fixture)
    wait(lambda: cell(name) is not None, 'navigate back to the original directory')
    key('ctrl+v')
    moved = fixture / renamed_name
    wait(lambda: not renamed.exists() and has_payload(moved, payload) and cell(renamed_name) is not None,
         'move the selected file through the real clipboard')
    snapshot('moved')
    stage = 'trash'
    select(renamed_name)
    key('Delete')
    wait(lambda: not moved.exists() and len(trash_matches(moved, payload)) == 1,
         'move the selected file into actual Trash with its original path and bytes')
    trash_uris = trash_matches(moved, payload)
    if not has_payload(seed, payload):
        raise RuntimeError('Trash affected the unselected original file')
    snapshot('trashed')
    stage = 'undo-trash'
    key('ctrl+z')
    wait(lambda: has_payload(moved, payload) and not trash_matches(moved, payload),
         'restore the same file through its real Undo action')
    snapshot('restored')
    records.append({'uid': os.getuid(), 'app_id': 'org.gnome.Nautilus',
                    'browser_window_id': browser_id, 'process': identity, 'fixture': str(fixture),
                    'file_sha256': hashlib.sha256(payload).hexdigest(),
                    'bytes': len(payload), 'trash_uris': trash_uris,
                    'passed': ['native-content', 'create-folder', 'copy', 'rename',
                               'move', 'trash-original-path-and-bytes', 'undo-trash'],
                    'scope': 'ordinary native app in the reference container; final shipped image remains untested'})
    stage = 'close'
    key('ctrl+w')
    wait(lambda: all(window['id'] != browser_id for window in scene()['windows']),
         'close its native browser window')
    (OUT / 'nautilus-operations.json').write_text(json.dumps(records, indent=2))
    print('Nautilus genuine native file operations passed: create/copy/rename/move/trash/undo/close')
except Exception as error:
    (OUT / 'nautilus-operations-failure.json').write_text(json.dumps({'stage': stage, 'error': str(error), 'fixture': str(fixture) if fixture else None}, indent=2))
    try:
        snapshot('failure')
    except Exception as evidence_error:
        (OUT / 'nautilus-failure-evidence-error.txt').write_text(str(evidence_error))
    raise
