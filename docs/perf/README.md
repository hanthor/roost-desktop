# Marlin GNOME and Roost resource samples

Issue #73 requires comparable measured evidence before any performance claim. The `Marlin performance samples` workflow (manual dispatch or a pull request changing its measurement code) uses one runner, one shared Marlin GNOME image plus Roost payload, and the same 6 GiB / four-vCPU / 1280×800 virtio QEMU/KVM profile for both sessions. GNOME runs first, then Roost; one boot per session does not establish cold/warm variance. The input package must come from a successful trusted main CI run. The workflow records package SHA256, package source/run, base digest, shared image ID, observer revision, host CPU/kernel and QEMU version.

The published Marlin tag was measured as GNOME Shell 50.5 at digest `sha256:3311d7784a0b0e7cd5486d8b69d974f4a34c3436b9812599470d00f80f805f1b` in [run 37176268295](https://github.com/hanthor/roost-desktop/actions/runs/37176268295). It is not the GNOME 51 baseline claimed by older roadmap prose. This benchmark derives a CI-only baseline by enabling Arch core-testing and extra-testing together, performing a full system upgrade, and requiring GNOME Shell 51. Both sessions use that same derived payload; its package versions and shared image ID are retained. This does not change the shipping Marlin image. The first successful paired guest run is recorded in the dated report below.

The CI-only observer reads every process with the test user's UID, including activated portals and session services. It records per-process identity/start time, CPU counters, PSS/RSS, fds and process count once per second. The observer and its descendants are excluded; root/GDM services, kernel allocations and GPU buffers are outside this user scope. Reading protected `smaps_rollup` requires the observer to run as root. An incomplete read is a failed measurement, not zero memory.

The host waits for an actual rendered black panel, keeps 30 seconds of idle, opens Disks, System Monitor and Files, toggles the overview ten times, switches workspaces ten times, then keeps a final 30-second settle. Raw serial, resource samples, input observations, screenshots and per-session reports are retained for 14 days. The p50/p95/p99 summaries state sample counts; whole-run CPU summaries include startup and workload. Guest timestamps and host phase timestamps are both retained and are not silently treated as synchronized clocks.

Input timings bound the response observed through QMP captures. Each observation records a conservative lower bound, upper bound and capture duration. QMP key injection, capture/poll overhead and host scheduling affect these measurements; they are **not** compositor input-to-frame tracing or a frame-pacing measurement. The software/guest path, only one boot, missing GPU memory, missing notification workload, and missing 24-hour soak remain limitations. No 10-percent release gate is introduced here; that requires the ADR/change-control process and the complete benchmark evidence described in [research/README.md](../research/README.md).

Run a fixture disk with `scripts/roost-vm-perf --disk disk.raw --desktop gnome --out /tmp/gnome-perf-fresh`, or select `roost` for the candidate. Each artifact directory must be new. Build `packaging/marlin/perf/Containerfile` with `DESKTOP=gnome` or `DESKTOP=roost` on the same shared preview image; it changes only the test user, autologin, serial logging and observer. The fixture never ships in a desktop image.

The [2026-10-04 paired report](2026-10-04-marlin-gnome51/README.md) records the first successful run, including the higher sampled Roost CPU and slower median observed overview response. Its package predates the latest roadmap work. Issue #73 remains open.

The paired lane also captures `wayland-info` from each actual interactive
Marlin session. The root observer invokes the tool as the fixture user and
retains the full output as digest-checked journal chunks; missing, partial,
duplicate or corrupted captures fail the run. Each desktop's artifact contains
`wayland-info.txt` and `wayland-info-source.json`, associated with the image,
package and observer provenance in that run. This supplies a native Marlin
protocol reference once a run passes; it does not replace the existing Fedora
headless reference without retaining and reviewing that actual evidence.
The comparison requires KVM and fails rather than accepting software emulation.
