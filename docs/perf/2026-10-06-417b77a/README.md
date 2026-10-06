# Paired GNOME 51 and Roost samples at 417b77a

The first paired measurement of the merged idle-rendering changes completes, but does not establish release performance parity. Roost's idle CPU is low in this run; workload CPU and median client presentation cadence still need improvement. Preserve this run alongside future favorable or unfavorable results.

Measured 2026-10-06 in [workflow run 37517355092](https://github.com/tuna-os/tuna-desktop/actions/runs/37517355092), using successful main package run 37512958838. Candidate and observer source: 417b77a9a0602491a853c60f618e0f4720ba9e1a. This predates the later input-recovery and portal-readiness merges and the pending buffer-age/tracing stack. It is not a measurement of the final shipped package.

Both desktops use one shared upgraded Marlin payload, KVM, four vCPUs, 6144 MiB RAM, 1280×800 and virtio-vga-pcie-pm-preserved-v1 on the same AMD EPYC 9V45 runner (host kernel 7.0.0-1012-azure). Reference is GNOME Shell 51.0, confirmed in gnome-version. Base digest, shared image ID, package checksum and the exact guest package inventory are adjacent files. The reported output refresh is about 13.334 ms (75Hz); this does not qualify physical 60/120 Hz configurations.

| Metric | GNOME 51 | Roost |
|---|---:|---:|
| Panel observed upper bound (s; one boot each) | 22.56 | 20.54 |
| Idle CPU mean (% of one CPU; 29 intervals each) | 0.31 | 0.48 |
| Idle CPU p50 (%) | 0.00 | 0.00 |
| Idle CPU p95 (%) | 3.00 | 2.00 |
| Idle CPU p99 (%) | 5.00 | 2.00 |
| Whole-user CPU mean (%; startup and workload included) | 16.02 | 35.55 |
| Whole-user PSS p50 (MiB) | 901.39 | 835.90 |
| Whole-user PSS p95 (MiB) | 935.94 | 1007.93 |
| Whole-user PSS p99 (MiB) | 936.45 | 1008.12 |
| Overview observed p50 (ms; 20 observations each) | 160.98 | 315.58 |
| Overview observed p95 (ms) | 412.69 | 583.82 |
| Overview observed p99 (ms) | 488.76 | 604.75 |
| Native presentation interval p50 (ms; 239 intervals each) | 13.33 | 26.67 |
| Native presentation interval p95 (ms; 239 intervals each) | 26.67 | 26.67 |
| Native presentation interval p99 (ms; 239 intervals each) | 133.34 | 93.34 |

CPU percentages use one CPU as 100%; four fully occupied vCPUs would total 400%. Idle uses only guest-clock intervals wholly inside the 30-second idle phase, after the equal post-panel settle. Whole-user CPU has 198/203 intervals and PSS 199/204 samples for GNOME/Roost respectively, including startup and workload. PSS covers the test UID and excludes root/GDM and the observer with its descendants. This scope must not be relabeled total system memory.

Roost's PSS p95 is 7.69% above the reference in this pair. The observed overview upper-bound p95 is 41.47% higher. QMP observations are host-visible upper bounds and do not satisfy the required compositor input-to-frame tracing gate. Both native clients have 240 actual presentations with vsync, hardware-clock and completion flags; GNOME retains two pending commits at collection, Roost none. Median intervals differ by one reported refresh period, while GNOME's p99 is higher. These are cadence observations, not proof of the cause or animation completion. DRM memory counters are unavailable for every sample, not zero. Both notification services accepted ten requests; acceptance is not notification latency.

## Evidence and reproduction

The adjacent per-desktop reports, raw compressed resource samples, individual presentation records, overview observations, guest phase markers and Wayland global capture retain the measurement data. samples.json.gz decompresses to the exact raw JSON bytes; compression has a fixed timestamp. The workflow artifact also contains original serial logs and rendered observations. Its retention is 14 days; the numerical traces retained here remain reviewable after that artifact expires.

Executed command, while main was 417b77a:

```sh
gh workflow run performance-baseline.yml --ref main -f package_run=37512958838
```

The selected package has SHA256 88533ecb167a195d366dc3036b0e2b6dde0b621ebaec275115ab3c4d58823b06. Future dispatches must identify their observer revision, package run, shared image and software inventory; a moving main or testing repository is not automatically the same experiment.

Remaining #73/#203/#315 work: repeated cold/warm paired trials and variance, compositor input tracing for both desktops, physical GPU/output qualification and usable GPU-memory accounting, the final signed package/published image, and an actual 24-hour memory/cadence soak. The existing 10% latency/PSS targets and no-pacing-regression requirement are unchanged. No release gate or production-readiness claim passes on this single pair.
