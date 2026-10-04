#!/usr/bin/python3
"""Add newer fixture bindings without dropping an older host's bindings."""
from pathlib import Path
import sys
import xml.etree.ElementTree as ET

from gi.repository import Gio

required = {
    "screen-brightness-up-monitor": ["<Shift>XF86MonBrightnessUp"],
    "screen-brightness-down-monitor": ["<Shift>XF86MonBrightnessDown"],
    "screen-brightness-cycle": ["XF86MonBrightnessCycle"],
    "screen-brightness-cycle-monitor": ["<Shift>XF86MonBrightnessCycle"],
}
source = Gio.SettingsSchemaSource.get_default()
schema = source.lookup("org.gnome.shell.keybindings", True)
if schema is None:
    raise SystemExit("the shell fixture must supply its baseline schema first")
keys = set(schema.list_keys())
if required.keys() <= keys:
    raise SystemExit(0)

root = ET.Element("schemalist")
replacement = ET.SubElement(root, "schema", id=schema.get_id(), path=schema.get_path())
for name in sorted(keys):
    original = schema.get_key(name)
    # Shell keybindings are unconstrained string arrays. Refuse to silently
    # discard constraints if a future host changes that contract.
    if original.get_value_type().dup_string() != "as" or original.get_range().unpack()[0] != "type":
        raise SystemExit("unexpected constrained/non-array shell binding: " + name)
    key = ET.SubElement(replacement, "key", name=name, type="as")
    ET.SubElement(key, "default").text = original.get_default_value().print_(True)
for name, default in required.items():
    if name not in keys:
        key = ET.SubElement(replacement, "key", name=name, type="as")
        ET.SubElement(key, "default").text = repr(default)
directory = Path(sys.argv[1])
directory.mkdir(parents=True, exist_ok=True)
ET.ElementTree(root).write(directory / "org.roost.fixture-keybindings.gschema.xml", encoding="unicode")
