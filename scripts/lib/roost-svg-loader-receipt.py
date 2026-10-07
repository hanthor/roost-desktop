#!/usr/bin/python3
"""Fail-visible actual candidate SVG loader and native font inventory."""
import hashlib
import json
from pathlib import Path
import stat
import subprocess
import gi
gi.require_version('GdkPixbuf', '2.0')
from gi.repository import GdkPixbuf

OUT = Path('/out')
HELPER = Path('/candidate/usr/bin/roost-wallpaper-svg')
metadata = HELPER.stat()
if metadata.st_uid != 0 or metadata.st_mode & 0o022 or not stat.S_ISREG(metadata.st_mode):
    raise RuntimeError('actual packaged SVG helper must be root-owned and immutable')
formats = [f.get_name() for f in GdkPixbuf.Pixbuf.get_formats()]
if 'svg' not in formats:
    raise RuntimeError('actual installed native SVG loader unavailable')
output = subprocess.run([str(HELPER), '--receipt'], stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, check=True, timeout=6)
receipt = json.loads(output.stdout)
observed = subprocess.check_output([str(HELPER), '--identity'], text=True, timeout=6).strip()
if receipt['sha256'] != observed or len(observed) != 64:
    raise RuntimeError('actual helper loader/font identity changed during preflight')
packages = set()
for resource in receipt['resources']:
    path = Path(resource['path'])
    if path.stat().st_size != resource['bytes'] or hashlib.sha256(path.read_bytes()).hexdigest() != resource['sha256']:
        raise RuntimeError('actual backend resource differs from helper observation')
    if path != HELPER:
        package = subprocess.check_output(['rpm', '-qf', '--qf', '%{NAME}', str(path)], text=True)
        packages.add(package)
        resource['rpm_owner'] = subprocess.check_output(['rpm', '-q', package], text=True).strip()
paths = [r['path'] for r in receipt['resources']]
if not any('libpixbufloader-svg' in p for p in paths) or not any('librsvg' in p for p in paths):
    raise RuntimeError('candidate did not load actual GNOME SVG loader/librsvg')
if not any('libfontconfig' in p for p in paths) or not any(p.endswith(('.otf', '.ttf', '.ttc')) for p in paths):
    raise RuntimeError('candidate did not observe actual native font resources')
verification = []
for package in sorted(packages):
    result = subprocess.run(['rpm', '-V', package], capture_output=True, text=True)
    verification.append({'package': package, 'returncode': result.returncode,
                         'stdout': result.stdout, 'stderr': result.stderr})
    if result.returncode or result.stdout or result.stderr:
        (OUT/'svg-package-verification.json').write_text(json.dumps(verification, indent=2))
        raise RuntimeError('actual native loader/font package verification failed')
(OUT/'svg-package-verification.json').write_text(json.dumps(verification, indent=2))
receipt['helper'] = {'path': str(HELPER), 'uid': metadata.st_uid,
                     'sha256': hashlib.sha256(HELPER.read_bytes()).hexdigest(),
                     'version': subprocess.check_output([str(HELPER), '--version'], text=True).strip()}
receipt['reference_gdk_pixbuf_formats'] = formats
(OUT/'svg-loader-receipt.json').write_text(json.dumps(receipt, indent=2))
print('actual native SVG loader/font/helper preflight passed')
