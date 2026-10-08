# Native presentation and idle CPU, 2026-10-04

[Paired run 37200994800](https://github.com/tuna-os/tuna-desktop/actions/runs/37200994800) completed on one KVM runner using the same four-vCPU, 6 GiB, 1280×800 profile and shared image for both sessions. GNOME ran first, followed by Roost. Each session booted once. The derived baseline contains GNOME Shell/Mutter 51.0; the published Marlin base before its CI-only upgrade was 50.5.

The Roost package came from successful trusted-main [run 37189354041](https://github.com/tuna-os/tuna-desktop/actions/runs/37189354041), source `7546647e5967825f522a352ee781e0908f889d1e`. It predates the idle retention and buffer-age damage fixes proposed after this measurement. This is a measured baseline, not qualification of the latest roadmap source.

| Metric | GNOME 51 | Roost | Samples per session |
|---|---:|---:|---:|
| Native presentation interval p50, ms | 13.334 | 26.669 | 239 |
| Native presentation interval p95, ms | 26.669 | 26.669 | 239 |
| Native presentation interval p99, ms | 40.004 | 133.344 | 239 |
| Pure idle user CPU p50, percent | 0 | 127.982 | 29 |
| Pure idle user CPU p95, percent | 0 | 142.976 | 29 |
| Pure idle user CPU p99, percent | 1.000 | 144.981 | 29 |
| Pure idle user CPU mean, percent | 0.034 | 130.293 | 29 |
| Accepted notification IDs | 10 | 10 | 10 |

The native probe paints alternating SHM buffers in a 320×180 xdg-toplevel, schedules commits from actual surface frame callbacks, and retains 240 `wp_presentation` feedback records. Both virtual Bochs outputs report approximately 13.334 ms refresh intervals. Every record reports vsync, hardware clock and hardware completion, with zero zero-copy flags and zero unknown refresh values. These are the virtual DRM driver’s protocol flags, not evidence about physical GPUs. GNOME submitted 242 commits: 240 presented and two still pending when collection stopped. Roost submitted 240, all presented. Neither trace records discarded feedback.

Idle follows a 30-second settle and lasts 30 seconds. Only complete one-second CPU intervals within the guest CLOCK_BOOTTIME boundaries qualify. CPU sums every surviving process of the interactive UID; 100 percent represents one core. The root observer and its descendants are excluded. Startup, the frame probe, application launches, overview toggles and notifications lie outside this idle interval. The complete resource reports also include startup/workload samples and must not be interpreted as pure idle statistics.

Both notification workloads called the actual interactive user’s Notifications service and received ten distinct, nonzero uint32 IDs. Acceptance does not measure notification presentation latency. All 207 GNOME and 210 Roost DRM memory samples report unavailable counters; unavailable is not zero GPU memory.

The retained raw traces, per-process samples, phase markers, notification records, observed response bounds and reports are losslessly compressed. `manifest.json` gives compressed and decompressed SHA256 values. Package, baseline and image provenance are retained alongside them. Run `python3 docs/perf/2026-10-04-native-cadence/reproduce.py` from the repository root to verify every retained file and recompute presentation and idle percentiles from the raw samples.

Observer source is `efa1dd33bb0304fa0ab6469a2048486b517980b4`, the actual workflow checkout containing the measurement implementation. Shared image ID is `4d877cd235a22039bf4139b172d204633feab559be6f32742a1b7e68259a978f`; base digest is recorded in `base-digest`. The host used AMD EPYC 7763, Linux 6.17.0-1022-azure and QEMU 8.2.2. Exact package lists, per-session manifests and source package hash accompany this report.

Roost’s median cadence and high idle CPU need improvement. No 10-percent release threshold is introduced by this report. Compositor input-to-frame tracing, repeated cold/warm boots, a 24-hour soak, physical GPU qualification and candidate measurements after the proposed repaint changes remain open under #73. QMP screenshot response bounds in the raw observations remain host-observed upper bounds; they are not compositor input-to-frame latency.
