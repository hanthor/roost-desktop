import json,os,time,pathlib,hashlib
names={'roost-compositor','roost-shell-gtk'}
def read():
    out={}
    for p in pathlib.Path('/proc').iterdir():
        if not p.name.isdigit(): continue
        try:
            name=pathlib.Path(os.readlink(p/'exe')).name
            if name not in names or p.stat().st_uid !=1000: continue
            f=(p/'stat').read_text().rsplit(')',1)[1].split()
            out[name]={'pid':int(p.name),'start':int(f[19]),'ticks':int(f[11])+int(f[12])}
        except (FileNotFoundError,ProcessLookupError): continue
    if set(out)!=names: raise RuntimeError('critical process missing')
    return out
print(json.dumps({'scope':'short idle critical-process CPU probe, not GNOME parity or endurance','source':'68dc3db9a6672d24c027245fa8284ab5b714cc79','boot_id':pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'kernel':os.uname().release,'clock_ticks':os.sysconf('SC_CLK_TCK'),'binaries':{n:hashlib.sha256(pathlib.Path('/usr/bin',n).read_bytes()).hexdigest() for n in names}}),flush=True)
before=read(); at=time.monotonic()
for i in range(15):
    time.sleep(2); after=read(); now=time.monotonic()
    if any(after[n]['pid']!=before[n]['pid'] or after[n]['start']!=before[n]['start'] for n in names): raise RuntimeError('process restarted')
    print(json.dumps({'sample':i,'interval_s':now-at,'cpu_percent':{n:(after[n]['ticks']-before[n]['ticks'])/os.sysconf('SC_CLK_TCK')/(now-at)*100 for n in names}}),flush=True)
    before,at=after,now
