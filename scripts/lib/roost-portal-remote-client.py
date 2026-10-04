#!/usr/bin/python3
"""Real GNOME portal consent, linked screen cast, legacy input and libei delivery."""
import json
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import time
from gi.repository import Gio, GLib
dump_spec = importlib.util.spec_from_file_location("roost_pipewire_dump", Path(__file__).with_name("roost-pipewire-dump.py"))
dump_module = importlib.util.module_from_spec(dump_spec)
dump_spec.loader.exec_module(dump_module)
out = Path(sys.argv[1])
mode = sys.argv[2]
interface = 'org.freedesktop.portal.RemoteDesktop'
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
responses = {}
def response(_conn, _sender, path, _iface, _signal, params, _data):
    responses[path] = params.unpack()
bus.signal_subscribe(None, 'org.freedesktop.portal.Request', 'Response', None, None,
                     Gio.DBusSignalFlags.NONE, response, None)
def call(iface, method, args, path='/org/freedesktop/portal/desktop', connection=bus):
    return connection.call_sync('org.freedesktop.portal.Desktop', path, iface, method,
                                args, None, Gio.DBusCallFlags.NONE, 30000, None).unpack()
def request(iface, method, signature, args):
    handle, = call(iface, method, GLib.Variant(signature, args))
    if method == 'Start': out.with_suffix('.ready').write_text(mode)
    deadline = time.monotonic() + 60
    while handle not in responses and time.monotonic() < deadline:
        GLib.MainContext.default().iteration(False)
        time.sleep(.01)
    if handle not in responses: raise RuntimeError(method + ' timed out')
    return responses.pop(handle)
def options(token): return {'handle_token': GLib.Variant('s', token)}
opts = options('create')
opts['session_handle_token'] = GLib.Variant('s', 'remoteproof' + str(os.getpid()))
code, result = request(interface, 'CreateSession', '(a{sv})', (opts,))
if code and mode == 'locked':
    out.with_suffix('.response.json').write_text(json.dumps({'mode': mode, 'response': code, 'stage': 'CreateSession'}, indent=2))
    print('locked denied at CreateSession with response ' + str(code))
    sys.exit(0)
if code: raise RuntimeError('CreateSession failed ' + str(code))
session = result['session_handle']
opts = options('devices')
opts['types'] = GLib.Variant('u', 3)
code, _ = request(interface, 'SelectDevices', '(oa{sv})', (session, opts))
if code: raise RuntimeError('SelectDevices failed')
opts = options('sources')
opts.update({'types': GLib.Variant('u', 1), 'multiple': GLib.Variant('b', False),
             'cursor_mode': GLib.Variant('u', 1)})
code, _ = request('org.freedesktop.portal.ScreenCast', 'SelectSources', '(oa{sv})', (session, opts))
if code: raise RuntimeError('SelectSources failed')
code, result = request(interface, 'Start', '(osa{sv})', (session, '', options('start')))
out.with_suffix('.response.json').write_text(json.dumps({'mode': mode, 'response': code, 'results': result}, indent=2))
if mode in ('cancel', 'locked'):
    if code == 0: raise RuntimeError(mode + ' unexpectedly granted remote control')
    print(mode + ' denied with response ' + str(code))
    sys.exit(0)
if code != 0 or result.get('devices') != 3: raise RuntimeError('keyboard/pointer grant missing')
node = result['streams'][0][0]
reply, fds = bus.call_with_unix_fd_list_sync('org.freedesktop.portal.Desktop', '/org/freedesktop/portal/desktop',
    'org.freedesktop.portal.ScreenCast', 'OpenPipeWireRemote', GLib.Variant('(oa{sv})', (session, {})),
    None, Gio.DBusCallFlags.NONE, 30000, None, None)
fd = fds.get(reply.unpack()[0])
try:
    subprocess.run(['gst-launch-1.0', '-q', 'pipewiresrc', 'fd=' + str(fd), 'path=' + str(node),
                    'num-buffers=1', '!', 'videoconvert', '!', 'pngenc', '!', 'filesink',
                    'location=' + str(out)], pass_fds=(fd,), check=True, timeout=30)
finally: os.close(fd)
other = Gio.DBusConnection.new_for_address_sync(os.environ['DBUS_SESSION_BUS_ADDRESS'],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def input_call(method, signature, args, connection=bus):
    return call(interface, method, GLib.Variant(signature, (session, {}, *args)), connection=connection)
try:
    input_call('NotifyKeyboardKeycode', '(oa{sv}iu)', (30, 1), other)
except GLib.Error: pass
else: raise RuntimeError('foreign owner injected keyboard input')
before_legacy = json.loads(Path('/out/input.json').read_text())
input_call('NotifyPointerMotionAbsolute', '(oa{sv}udd)', (node, 640., 400.))
input_call('NotifyPointerButton', '(oa{sv}iu)', (0x110, 1))
input_call('NotifyPointerButton', '(oa{sv}iu)', (0x110, 0))
input_call('NotifyKeyboardKeycode', '(oa{sv}iu)', (48, 1))
input_call('NotifyKeyboardKeycode', '(oa{sv}iu)', (48, 0))
def delivered(keycode, before):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        state = json.loads(Path('/out/input.json').read_text())
        if (any(key['keycode'] == keycode for key in state['keys'][len(before['keys']):])
                and len(state['buttons']) > len(before['buttons'])):
            return state
        time.sleep(.05)
    raise RuntimeError('input was accepted but not delivered to the Wayland GTK client: ' + str(state))
out.with_suffix('.legacy-input.json').write_text(json.dumps(delivered(56, before_legacy)))
if mode == 'eis':
    before_eis = json.loads(Path('/out/input.json').read_text())
    reply, fds = bus.call_with_unix_fd_list_sync('org.freedesktop.portal.Desktop', '/org/freedesktop/portal/desktop',
        interface, 'ConnectToEIS', GLib.Variant('(oa{sv})', (session, {})), None,
        Gio.DBusCallFlags.NONE, 30000, None, None)
    fd = fds.get(reply.unpack()[0])
    try: subprocess.run(['/out/remote-ei', str(fd)], pass_fds=(fd,), check=True, timeout=15)
    finally: os.close(fd)
    out.with_suffix('.eis-input.json').write_text(json.dumps(delivered(38, before_eis)))
elif mode in ('revoke', 'backend-disconnect'):
    # Require a clean seat baseline; expose only the aggregate count.
    telemetry = json.loads(Path('/out/compositor-state.json').read_text())
    if telemetry['seat_pressed_key_count'] != 0: raise RuntimeError('seat baseline is not empty')
    # Leave Shift held so lock/backend-loss must release real seat state.
    input_call('NotifyKeyboardKeycode', '(oa{sv}iu)', (42, 1))
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        telemetry = json.loads(Path('/out/compositor-state.json').read_text())
        if telemetry['seat_pressed_key_count'] == 1: break
        time.sleep(.05)
    else: raise RuntimeError('held remote Shift never reached seat state')
    out.with_suffix('.active').write_text(str(node))
    deadline = time.monotonic() + 30
    while not out.with_suffix('.revoke').exists() and time.monotonic() < deadline: time.sleep(.01)
    if not out.with_suffix('.revoke').exists(): raise RuntimeError('runner did not revoke grant')
elif mode == 'disconnect': bus.close_sync(None)
else: call('org.freedesktop.portal.Session', 'Close', None, session)
deadline = time.monotonic() + 2
while time.monotonic() < deadline:
    raw_dump = subprocess.check_output(["pw-dump", "--no-colors"], timeout=1)
    out.with_suffix(".pw-dump.txt").write_bytes(raw_dump)
    nodes = dump_module.dump_objects(raw_dump)
    if time.monotonic() >= deadline:
        raise RuntimeError("PipeWire withdrawal exceeded two seconds")
    if not any(item.get('id') == node and item.get('type') == 'PipeWire:Interface:Node' for item in nodes):
        if mode in ('revoke', 'backend-disconnect'):
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                telemetry = json.loads(Path('/out/compositor-state.json').read_text())
                if telemetry['seat_pressed_key_count'] == 0: break
                time.sleep(.05)
            else: raise RuntimeError('revocation left remote Shift held in seat state')
            # Backend loss is observed by the bounded registry poll. Check
            # post-revocation admission only once stream and seat teardown are
            # observable, rather than racing the triggering kill/lock call.
            before_revoked_input = json.loads(Path('/out/input.json').read_text())
            # The GNOME frontend may acknowledge its fire-and-forget notify
            # even after the backend is gone. Actual seat/client delivery is
            # the admission oracle, not that outer void-method reply.
            try: input_call('NotifyKeyboardKeycode', '(oa{sv}iu)', (30, 1))
            except GLib.Error: pass
            denial_deadline = time.monotonic() + 2
            while time.monotonic() < denial_deadline:
                state = json.loads(Path('/out/input.json').read_text())
                telemetry = json.loads(Path('/out/compositor-state.json').read_text())
                if state['keys'] != before_revoked_input['keys'] or telemetry['seat_pressed_key_count'] != 0:
                    raise RuntimeError('revoked input reached real seat/client state')
                time.sleep(.05)
            out.with_suffix('.revoked-input.json').write_text(json.dumps(state))
        print('real portal remote keyboard/pointer delivered; revoked node absent; held seat state cleared')
        break
    time.sleep(.05)
else: raise RuntimeError('revocation retained linked screen-cast node')
