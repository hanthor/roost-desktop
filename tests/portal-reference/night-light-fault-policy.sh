#!/usr/bin/env bash
# Source-policy tests as the helper's actual required UID1000, not the runner UID.
# This is never GPU/provider/package-runtime qualification.
set -euo pipefail
[ "$(id -u)" -eq 0 ]
[ "$(id -u roost-proof)" -eq 1000 ]
[ "$#" -eq 0 ]
work=$(mktemp -d /tmp/roost-night-light-policy.XXXXXXXX)
trap 'rm -rf "$work"' EXIT
python3 - "$work" <<'COPY'
import hashlib,json,os,stat,sys
from pathlib import Path
root=Path(sys.argv[1])
paths=['scripts/lib/roost-night-light-ready.py','scripts/lib/night-light-ready-tests.py','scripts/lib/night-light-fault-tests.py','packaging/marlin/vm-lane/roost-vm-night-light-fault',
       'crates/compositor/Cargo.toml','crates/compositor/src/lib.rs','packaging/arch/PKGBUILD']
receipts=[]
key=lambda s:(s.st_dev,s.st_ino,s.st_uid,s.st_mode,s.st_size,s.st_mtime_ns,s.st_ctime_ns)
for name in paths:
    source=Path('/repo')/name
    fd=os.open(source,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(fd,'rb') as stream:
        before=os.fstat(stream.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0<before.st_size<=1024*1024:
            raise RuntimeError('fixed bounded public source required')
        data=stream.read(1024*1024+1);after=os.fstat(stream.fileno())
    if len(data)!=before.st_size or key(before)!=key(after) or key(after)!=key(source.lstat()):
        raise RuntimeError('original source changed during fixed copy')
    target=root/name
    target.parent.mkdir(parents=True,exist_ok=True)
    for directory in target.parents:
        if directory == root.parent: break
        if directory.stat().st_uid != 0: raise RuntimeError('root-owned copied source directories required')
        directory.chmod(0o755)
    target.write_bytes(data)
    target.chmod(0o555 if name.endswith('/roost-vm-night-light-fault') else 0o444)
    if target.stat().st_uid!=0 or target.read_bytes()!=data:
        raise RuntimeError('protected exact copy required')
    receipts.append({'path':name,'sha256':hashlib.sha256(data).hexdigest(),'size':len(data),'copy_uid':0,'copy_mode':oct(stat.S_IMODE(target.stat().st_mode))})
root.chmod(0o755)
Path('/out/fault-policy-source-receipts.json').write_text(json.dumps({'source_only':True,'files':receipts},indent=2)+'\n')
COPY
runuser -u roost-proof -- id > /out/fault-policy-identity.txt
runuser -u roost-proof -- python3 "$work/scripts/lib/night-light-fault-tests.py" > /out/fault-policy-tests.txt 2>&1
runuser -u roost-proof -- python3 "$work/scripts/lib/night-light-ready-tests.py" > /out/readiness-policy-tests.txt 2>&1
printf '%s\n' 'Actual UID1000 source-policy suites passed; no GPU/provider qualification.' > /out/fault-policy-scope.txt
