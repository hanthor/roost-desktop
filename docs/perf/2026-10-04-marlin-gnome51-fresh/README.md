# Fresh paired GNOME 51 and Roost observations

Run [37188800170, attempt 2](https://github.com/hanthor/roost-desktop/actions/runs/37188800170/attempts/2) passed on 2026-10-04. The observer is PR #251 at `3fd269a5e2df682ffd77db84e56f0918144b34b8`. Its package came from successful whole main-push CI [37189354041](https://github.com/hanthor/roost-desktop/actions/runs/37189354041), source `7546647e5967825f522a352ee781e0908f889d1e`, SHA256 `58cfb1792e028aff6d511156e9292a83296934d550ae223538113d6c6056bcb6`.

Both sessions ran sequentially on the same AMD EPYC 7763 host with QEMU 8.2.2 and KVM, four CPUs, 6 GiB RAM and 1280×800 output. Shared payload and base image identifiers, GNOME version, installed reference packages and package provenance are retained alongside this report. The complete run artifact also contains serial logs and all screenshots.

| Observation | GNOME 51 | Roost |
| --- | ---: | ---: |
| Full-run user PSS median, KiB | 900898 | 906887 |
| Full-run user PSS p95, KiB | 925253 | 975827 |
| Full-run user CPU median, % of one core | 19.00 | 158.97 |
| Full-run user CPU p95, % of one core | 79.99 | 232.87 |
| Overview visible-response upper bound median, ms | 158.95 | 381.97 |
| Overview visible-response upper bound p95, ms | 427.04 | 418.54 |

Resource samples mix startup, idle and workload: 155 GNOME and 160 Roost samples, with one fewer CPU delta each. Each desktop has 20 QMP overview observations. CPU values can exceed 100% because one core is 100%. Host-observed panel startup bounds were 24.56 and 24.57 seconds and include host scheduling and observation overhead.

These QMP measurements bound visible response; they do not measure input-to-frame latency or frame pacing. No GPU memory, synchronized pure-idle interval, repeated cold/warm distribution or 24-hour soak is qualified here. Roost used more CPU in this sampled workload. Issue #73 remains open; no release threshold changes.

The JSON reports retain exact statistics and environment metadata. Per-process raw samples are compressed without changing their contents; `uncompressed-sha256.json` records hashes of the original JSON bytes. Read them with `gzip -dc gnome-samples.json.gz` or `gzip -dc roost-samples.json.gz`.
