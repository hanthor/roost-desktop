# Short DRM frame-callback CPU probe

PR [#263](https://github.com/tuna-os/tuna-desktop/pull/263) corrected frame-callback accounting when a DRM page flip is already pending. This probe found no idle CPU improvement. It does not establish GNOME parity, frame pacing, GPU efficiency or endurance.

The same AWS KubeVirt VM ran both candidates on the same boot: four vCPUs, 6 GiB RAM, bochs-drm output at 1280×800 and approximately 75 Hz, kernel 7.2.8-arch1-2. The desktop was unlocked with no application workload. Both packages were temporarily installed in a transient `/usr` overlay above Marlin image `sha256:36dc326935943ef0651e4433307e5eae5c6abef730f082ab18ca072afc5f43e7`; this is not qualification of a published image. The GTK shell hash stayed identical. Exact candidate source revisions, measured binary hashes and boot ID are in the raw records.

Each run contains 15 approximately two-second intervals. CPU is `(utime + stime)` tick growth divided by clock ticks and measured monotonic elapsed time, as a percentage of one CPU core. Only the compositor and GTK shell for UID1000 are measured; root services, other session processes and kernel work are excluded. PID/start-time changes fail the observer. Percentiles use linear interpolation over these 15 samples; the short, sequential observation does not measure statistical significance.

| Candidate | Process | p50 | p95 | p99 | Population σ |
|---|---|---:|---:|---:|---:|
| before | roost-compositor | 86.38% | 92.14% | 92.69% | 2.89% |
| before | roost-shell-gtk | 0.50% | 1.00% | 1.00% | 0.34% |
| after | roost-compositor | 87.88% | 92.45% | 93.57% | 2.75% |
| after | roost-shell-gtk | 0.50% | 0.50% | 0.50% | 0.22% |

Raw records, summaries and the exact two observer scripts are retained here. Reproduction requires the same two built packages, a stable logged-in fixture UID1000 session, and running each observer as root. Changing the payload or VM profile makes it a new measurement.

Issue [#73](https://github.com/tuna-os/tuna-desktop/issues/73) stays open. Idle rendering still consumes most of one core in this profile; comparable input latency, frame pacing, GPU memory and a completed 24-hour soak remain unmeasured.
