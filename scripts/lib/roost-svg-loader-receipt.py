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
gi.require_version('Gly','2')
from gi.repository import Gly, Gio, GLib
formats = [f.get_name() for f in GdkPixbuf.Pixbuf.get_formats()]
if 'svg' not in formats:
    raise RuntimeError('actual installed native SVG loader unavailable')
output = subprocess.run([str(HELPER), '--receipt'], stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, check=True, timeout=6)
receipt = json.loads(output.stdout)
observed = subprocess.check_output([str(HELPER), '--identity'], text=True, timeout=6).strip()
if receipt['sha256'] != observed or len(observed) != 64:
    raise RuntimeError('actual helper loader/font identity changed during preflight')
# Actual primary loader objects keep their process alive through the receipt.
# After the one-use helper exits, the native parent-death chain must leave no
# live process with any of those exact owned identities. Do not kill by UID.
import time
cleanup=[]
for process in receipt['processors']:
    deadline=time.monotonic()+3
    while True:
        path=Path(f"/proc/{process['pid']}/stat")
        try:
            values=path.read_text().rsplit(')',1)[1].split()
            same=values[19]==process['start_ticks']
            terminated=not same or values[0]=='Z'
        except FileNotFoundError:
            terminated=True
        if terminated:break
        if time.monotonic()>=deadline:raise RuntimeError('actual owned Glycin process outlived helper')
        time.sleep(.02)
    cleanup.append({'pid':process['pid'],'start_ticks':process['start_ticks'],'no_live_original_process':True})
receipt['owned_processor_cleanup']=cleanup
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
if not receipt.get('modern_glycin') or not any('libglycin-2' in p for p in paths) or not any(p['executable'].endswith('/glycin-svg') for p in receipt['processors']):
    raise RuntimeError('candidate did not observe actual modern Glycin SVG stream loader')
if not any('/glycin-loaders/2+/conf.d/' in p for p in paths):
    raise RuntimeError('actual selected modern loader configuration unavailable')
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

# Real valid raster bytes distinguish a no-base rejection from an invalid image.
# GNOME Shell51 uses this same stream API, without a file/base URI.
import base64
import struct
raster=OUT/'external-svg-raster.png'
pixbuf=GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB,True,8,2,2)
pixbuf.fill(0xc80a14ff)
pixbuf.savev(str(raster),'png',[],[])
negative_proofs=[]
for kind,href,expected in [
    ('relative',raster.name,[0,0,0,0]),
    ('absolute',raster.as_uri(),[0,0,0,0]),
    ('embedded','data:image/png;base64,'+base64.b64encode(raster.read_bytes()).decode(),[200,10,20,255]),
]:
    source=("<svg xmlns='http://www.w3.org/2000/svg' width='2' height='2'>"
            f"<image href='{href}' width='2' height='2'/></svg>").encode()
    loader=Gly.Loader.new_for_stream(Gio.MemoryInputStream.new_from_bytes(GLib.Bytes.new(source)))
    loader.set_accepted_memory_formats(Gly.MemoryFormatSelection.R8G8B8A8)
    image=loader.load();frame=image.next_frame()
    if (frame.get_width(),frame.get_height(),frame.get_memory_format())!=(2,2,Gly.MemoryFormat.R8G8B8A8):
        raise RuntimeError('actual stream reference returned unexpected RGBA layout')
    raw=frame.get_buf_bytes().get_data();stride=frame.get_stride()
    reference=[list(raw[y*stride+x*4:y*stride+x*4+4]) for y in range(2) for x in range(2)]
    result=subprocess.run([str(HELPER)],input=source,capture_output=True,timeout=6,check=True,cwd=OUT)
    if len(result.stdout)!=32 or result.stdout[:8]!=b'RSVG0001' or struct.unpack('<II',result.stdout[8:16])!=(2,2):
        raise RuntimeError('actual candidate no-base SVG transport is invalid')
    candidate=[list(result.stdout[i:i+4]) for i in range(16,32,4)]
    record={'kind':kind,'source_sha256':hashlib.sha256(source).hexdigest(),
            'raster_sha256':hashlib.sha256(raster.read_bytes()).hexdigest(),
            'reference':reference,'candidate':candidate,'expected':expected,
            'candidate_stderr':result.stderr.decode(errors='replace')}
    negative_proofs.append(record)
    (OUT/'svg-no-base-stream-proof.json').write_text(json.dumps(negative_proofs,indent=2))
    if candidate!=reference or any(pixel!=expected for pixel in reference):
        raise RuntimeError('actual candidate/reference no-base or embedded-image contract differs')
