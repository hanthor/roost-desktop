# First paired Marlin GNOME 51 / Roost samples — 2026-10-04

[Measurement run 37178851584](https://github.com/tuna-os/tuna-desktop/actions/runs/37178851584) completed both actual guests on one runner. Each had 6 GiB, four vCPUs and a 1280×800 virtio display. GNOME Shell 51.0 ran first; Roost ran second, using the same CI-only full-system-upgraded Marlin payload. This is one boot per desktop.

The Roost package came from [trusted main run 37151532275](https://github.com/tuna-os/tuna-desktop/actions/runs/37151532275), source `c2efc2ac12b1919db24e6ddcd1440e8f23eab9c4`. It predates the current roadmap changes. Its SHA256, source record, shared image ID, base digest and installed package versions are retained beside this report. The measurement observer revision recorded by both reports is `1d0e4db298eca73e4fb87b337240205b0fdfaad3` (the pull-request test merge).

| Whole-run sampled metric | GNOME 51 | Roost |
| --- | ---: | ---: |
| Test-user PSS median, KiB (153 samples) | 889,907 | 941,647 |
| Test-user PSS p95, KiB | 912,850 | 993,205 |
| Observed test-user CPU median, percent (152 samples) | 16.0 | 148.0 |
| Observed test-user CPU p95, percent | 56.0 | 246.0 |
| Overview response upper bound median, ms (20 observations) | 158.1 | 345.2 |
| Overview response upper bound p95, ms | 356.3 | 354.5 |

CPU uses 100 percent per core. Samples include startup, idle, application launches, overview toggles and workspace switches; these numbers must not be labelled idle measurements. Host and guest phase clocks are not synchronized. PSS covers processes belonging to the test user, excluding the observer and its descendants; root/GDM services, GPU buffers and kernel memory are outside that scope. CPU can miss processes that exit between samples.

Overview times are host-visible bounds from QMP key injection and broad screenshot changes. They include capture, polling and scheduling overhead, and measure neither frame pacing nor compositor input-to-frame latency. The retained `*-overview-00.png` captures show both real overviews with the workload applications.

The higher sampled Roost CPU and slower median observed overview response require investigation. This run does not establish within-10-percent performance parity. Repeated cold/warm boots, synchronized phase analysis, notification workload, GPU memory/frame pacing and a 24-hour soak remain unmeasured. Issue #73 remains open. Raw serial logs and per-process samples are available in the linked run's `marlin-performance` artifact for its retention period; the checked-in summaries preserve the environmental and measurement limits.
