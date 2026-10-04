# Marlin GNOME and Roost resource samples

Issue #73 requires comparable measured evidence before any performance claim. The `Marlin performance samples` workflow (manual dispatch or a pull request changing its measurement code) uses one runner, one shared Marlin GNOME image plus Roost payload, and the same 6 GiB / four-vCPU / 1280×800 virtio QEMU/KVM profile for both sessions. GNOME runs first, then Roost; one boot per session does not establish cold/warm variance. The input package must come from a successful trusted main CI run. The workflow records package SHA256, package source/run, base digest, shared image ID, observer revision, host CPU/kernel and QEMU version.

The published Marlin tag was measured as GNOME Shell 50.5 at digest `sha256:3311d7784a0b0e7cd5486d8b69d974f4a34c3436b9812599470d00f80f805f1b` in [run 37176268295](https://github.com/hanthor/roost-desktop/actions/runs/37176268295). It is not the GNOME 51 baseline claimed by older roadmap prose. This benchmark derives a CI-only baseline by enabling Arch core-testing and extra-testing together, performing a full system upgrade, and requiring GNOME Shell 51. Both sessions use that same derived payload; its package versions and shared image ID are retained. This does not change the shipping Marlin image. The first successful paired guest run is recorded in the dated report below.

The CI-only observer reads every process with the test user's UID, including activated portals and session services. It records per-process identity/start time, CPU counters, PSS/RSS, fds and process count once per second. The observer and its descendants are excluded; root/GDM services and kernel allocations are outside this user scope. Reading protected `smaps_rollup` requires the observer to run as root. An incomplete read is a failed measurement, not zero memory.

DRM memory observations follow the [kernel's client usage format](https://www.kernel.org/doc/html/v6.9/gpu/drm-usage-stats.html). The observer retains only driver/device/client identity and standardized memory counters, normalized to bytes. Shared or duplicated handles for the same DRM client are counted once; distinct clients can still account the same shared buffer. Reports therefore label sums as **client-accounted bytes**, preserve each memory region and counter separately, and do not claim unique physical GPU usage. An unsupported driver reports `unavailable`; malformed or unreadable counters report `incomplete`. Neither contributes a measured zero. A supported counter explicitly containing zero remains a valid observation. These counters cover the test UID only and do not measure software renderer memory beyond PSS or memory owned by root services. Actual hardware accounting still requires a qualified run; virtual Bochs scanout may expose none.

The host waits for an actual rendered black panel, allows an equal 30-second post-panel settle, measures 30 seconds without input, opens Disks, System Monitor and Files, toggles the overview ten times, switches workspaces ten times, sends ten notifications one second apart, then keeps a final 30-second settle. Raw serial, resource samples, input observations, notification IDs, screenshots and per-session reports are retained for 14 days. The p50/p95/p99 summaries state sample counts; whole-run CPU summaries include startup and workload.

The CI-only guest agent runs only the fixed `/usr/libexec/roost-perf-phase` helper. It marks the idle boundaries with the guest's CLOCK_BOOTTIME, matching the observer's own clock. Idle CPU excludes any interval crossing either boundary, and fewer than 20 complete intervals fail the measurement. Host phase timestamps remain separate provenance. The fixture sends notifications to the actual interactive user's Notifications service and requires ten distinct nonzero uint32 IDs; the service's acceptance and first frame are retained. Acceptance is a workload record, not a claim of notification input-to-frame latency. Guest-agent packages, helper and service are fixture-only and installed identically in both sessions.

Input timings bound the response observed through QMP captures. Each observation records a conservative lower bound, upper bound and capture duration. QMP key injection, capture/poll overhead and host scheduling affect these measurements; they are **not** compositor input-to-frame tracing or a frame-pacing measurement. The software/guest path, only one boot, potentially unavailable DRM memory counters, and missing 24-hour soak remain limitations. No 10-percent release gate is introduced here; that requires the ADR/change-control process and the complete benchmark evidence described in [research/README.md](../research/README.md).

Run a fixture disk with `scripts/roost-vm-perf --disk disk.raw --desktop gnome --out /tmp/gnome-perf-fresh`, or select `roost` for the candidate. Each artifact directory must be new. Build `packaging/marlin/perf/Containerfile` with `DESKTOP=gnome` or `DESKTOP=roost` on the same shared preview image; it adds the test user, autologin, serial logging, observer and fixed guest probes. The fixture never ships in a desktop image.

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

The GNOME overview phase also retains the installed shell's own
[Sysprof capture and decoded scope marks](gnome-overview-sysprof.md), with peer
credentials, capture timestamps and a verified digest. These are raw event,
dispatch and presentation scopes; causal latency remains a separate analysis.
