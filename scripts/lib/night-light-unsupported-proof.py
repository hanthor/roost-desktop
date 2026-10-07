#!/usr/bin/env python3
"""Fixed unsupported Night Light assertion for the ordinary nested GTK journey.

This proves only capability/affordance absence. Genuine GNOME51 positive policy,
GPU/display pixels and untinted capture qualification remain separate mandatory
lanes. No provider is supplied and no setting is changed here.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat

NAME = 'org.gnome.Mutter.DisplayConfig'
PATH = '/org/gnome/Mutter/DisplayConfig'


def original_start(pid):
    if type(pid) is not int or pid <= 0 or os.getuid() <= 0:
        raise ValueError('ordinary original process required')
    process = Path(f'/proc/{pid}')
    if process.stat().st_uid != os.getuid():
        raise ValueError('original process UID changed')
    with (process/'stat').open() as stream:
        raw = stream.read(8193)
    if len(raw) > 8192 or not raw.startswith(str(pid)+' '):
        raise ValueError('original process stat refused')
    fields = raw.rsplit(')', 1)[1].split()
    value = int(fields[19])
    if value <= 0:
        raise ValueError('original start ticks refused')
    return value


def read_owned(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_uid != os.getuid() or not 0 < before.st_size <= limit:
            raise ValueError('owned fixture file type/UID/size refused')
        raw = stream.read(limit+1)
        after = os.fstat(stream.fileno())
        named = os.stat(path, follow_symlinks=False)
        identity = lambda value: (value.st_dev, value.st_ino, value.st_uid, value.st_mode, value.st_size, value.st_mtime_ns, value.st_ctime_ns)
        if len(raw) != before.st_size or identity(before) != identity(after) or identity(after) != identity(named):
            raise ValueError('owned fixture file changed during read')
    return json.loads(raw), {'sha256': hashlib.sha256(raw).hexdigest(), 'device': after.st_dev,
                             'inode': after.st_ino, 'bytes': len(raw)}


def unsupported_state(value):
    if type(value) is not dict or value.get('locked') is not False:
        raise ValueError('original normal unlocked state required')
    color = value.get('night_light')
    if type(color) is not dict or color.get('supported') is not False or color.get('service_supported') is not False:
        raise ValueError('actual unsupported color service required')
    if (type(color.get('owner_epoch')) is not int or color['owner_epoch'] != 0
            or type(color.get('generation')) is not int or color['generation'] <= 0
            or type(color.get('temperature')) is not int or color['temperature'] != 6500
            or color.get('service_rgb_scales') != [1.0,1.0,1.0]):
        raise ValueError('neutral unsupported service state required')
    rgb = color['service_rgb_scales']
    if any(type(item) not in (int,float) or isinstance(item,bool) for item in rgb):
        raise ValueError('neutral service RGB types refused')
    return {key: color[key] for key in ('supported','service_supported','owner_epoch','generation','temperature','service_rgb_scales')}


def absent_visible_tile(nodes):
    if type(nodes) is not list or not 1 <= len(nodes) <= 4096:
        raise ValueError('bounded actual accessible tree required')
    for node in nodes:
        if (type(node) is not dict or type(node.get('name')) is not str or len(node['name']) > 4096
                or type(node.get('showing')) is not bool):
            raise ValueError('actual accessible node schema refused')
        if node['showing'] and node['name'] == 'Night Light':
            raise ValueError('unsupported Night Light tile is visible')
    return len(nodes)


def prove(args):
    from gi.repository import Gio, GLib
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    uid = os.getuid()

    def call(method, signature, argument):
        return bus.call_sync('org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus',method,
                             GLib.Variant('(s)',(argument,)),GLib.VariantType.new(signature),
                             Gio.DBusCallFlags.NONE,2000,None).unpack()[0]
    owner = call('GetNameOwner','(s)',NAME)

    def guard():
        if (call('GetNameOwner','(s)',NAME) != owner
                or call('GetConnectionUnixProcessID','(u)',owner) != args.pid
                or call('GetConnectionUnixUser','(u)',owner) != uid
                or original_start(args.pid) != args.start
                or Path(f'/proc/{args.pid}/exe').resolve(strict=True) != Path(args.exe).resolve(strict=True)):
            raise ValueError('original DisplayConfig kernel principal changed')
    guard()
    result = bus.call_sync(owner,PATH,'org.freedesktop.DBus.Properties','Get',
                           GLib.Variant('(ss)',(NAME,'NightLightSupported')),GLib.VariantType.new('(v)'),
                           Gio.DBusCallFlags.NONE,2000,None).unpack()[0]
    if result is not False:
        raise ValueError('actual DisplayConfig must report unsupported')
    if call('NameHasOwner','(b)','org.gnome.SettingsDaemon.Color') is not False:
        raise ValueError('nested unsupported fixture unexpectedly has a real color service')
    state, state_receipt = read_owned(args.state,1024*1024)
    summary = unsupported_state(state)
    nodes, tree_receipt = read_owned(args.tree,1024*1024)
    count = absent_visible_tile(nodes)
    settings = Gio.Settings.new('org.gnome.settings-daemon.plugins.color')
    if settings.get_boolean('night-light-enabled') is not False:
        raise ValueError('unsupported proof changed original disabled key')
    guard()
    # Recheck the real property and key, not a callback echo from the widget.
    if bus.call_sync(owner,PATH,'org.freedesktop.DBus.Properties','Get',
                     GLib.Variant('(ss)',(NAME,'NightLightSupported')),GLib.VariantType.new('(v)'),
                     Gio.DBusCallFlags.NONE,2000,None).unpack()[0] is not False:
        raise ValueError('capability changed during unsupported observation')
    if settings.get_boolean('night-light-enabled') is not False:
        raise ValueError('original key changed during observation')
    if call('NameHasOwner','(b)','org.gnome.SettingsDaemon.Color') is not False:
        raise ValueError('real color owner appeared during observation')
    latest, state_receipt_after = read_owned(args.state,1024*1024)
    if unsupported_state(latest) != summary:
        raise ValueError('original unsupported normal state changed')
    guard()
    receipt = {'scope':'unsupported-negative-only','principal':{'owner':owner,'uid':uid,'pid':args.pid,'start':args.start},
               'display_config_supported':False,'enabled_key_unchanged':True,'visible_tile_absent':True,
               'accessible_nodes':count,'state_receipt':state_receipt,'state_receipt_after':state_receipt_after,'tree_receipt':tree_receipt,'neutral_service':summary}
    with Path(args.out).open('x') as stream:
        json.dump(receipt,stream,indent=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action',choices=('start','prove'))
    parser.add_argument('--pid',required=True,type=int)
    parser.add_argument('--start',type=int)
    for name in ('exe','state','tree','out'):parser.add_argument('--'+name)
    args = parser.parse_args()
    if args.action == 'start':
        print(original_start(args.pid))
    else:
        if args.start is None or args.start <= 0 or any(getattr(args,key) is None for key in ('exe','state','tree','out')):
            raise ValueError('fixed original proof inputs missing')
        prove(args)


if __name__=='__main__':main()
