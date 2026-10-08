#!/usr/bin/env bash
# Genuine RPM installation around a separately CI-built exact-source binary.
set -euo pipefail
[ "$(id -u)" -eq 0 ]
[ "$#" -eq 0 ]
mkdir -p /out /tmp/night-light-rpmbuild/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
cp /fixture/roost-vm-night-light-compositor /fixture/source-commit.txt /fixture/version.txt /fixture/binary-sha256.txt /tmp/night-light-rpmbuild/SOURCES/
cp /repo/tests/portal-reference/night-light-fixture.spec /tmp/night-light-rpmbuild/SPECS/
input_sha=$(sha256sum /fixture/roost-vm-night-light-compositor | cut -d' ' -f1)
[ "$(cut -d' ' -f1 /fixture/binary-sha256.txt)" = "$input_sha" ]
[ "$(cat /fixture/source-commit.txt)" = "$(cat /out/candidate-commit.txt)" ]
grep -F '[night-light-vm-fixture]' /fixture/version.txt
rpmbuild --define '_topdir /tmp/night-light-rpmbuild' -bb /tmp/night-light-rpmbuild/SPECS/night-light-fixture.spec > /out/fixture-rpmbuild.log 2>&1
mapfile -t rpms < <(find /tmp/night-light-rpmbuild/RPMS -name 'roost-night-light-fixture-*.rpm' -type f)
[ "${#rpms[@]}" -eq 1 ]
# Unsigned CI repack: authenticity is pinned by binary-sha256, source-commit,
# and installed-sha checks, not by a signature; rpm rejects unsigned payloads.
rpm -ivh --nosignature "${rpms[0]}" > /out/fixture-install.log 2>&1
rpm -q --qf '%{NEVRA}\n' roost-night-light-fixture > /out/fixture-nevra.txt
rpm -qf --qf '%{NAME}\n' /usr/libexec/roost-vm-night-light-compositor > /out/fixture-owner.txt
[ "$(cat /out/fixture-owner.txt)" = roost-night-light-fixture ]
rpm -V roost-night-light-fixture > /out/fixture-verify.txt
[ ! -s /out/fixture-verify.txt ]
installed_sha=$(sha256sum /usr/libexec/roost-vm-night-light-compositor | cut -d' ' -f1)
[ "$installed_sha" = "$input_sha" ]
/usr/libexec/roost-vm-night-light-compositor --version > /out/fixture-installed-version.txt
cmp /fixture/version.txt /out/fixture-installed-version.txt
sha256sum /usr/libexec/roost-vm-night-light-compositor "${rpms[0]}" > /out/fixture-installed-and-rpm-sha256.txt
cp "${rpms[0]}" /out/
cp /fixture/source-commit.txt /out/fixture-source-commit.txt
exec /repo/tests/portal-reference/night-light-run.sh --fixture-negative
