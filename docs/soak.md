# Actual guest endurance evidence

`scripts/roost-vm-soak` observes a running Roost session inside an isolated test VM for 24 hours by default. A newly booted GNOME base image is not a Roost soak. Install a qualified Roost payload, record its source revision and image digest, and establish the real session before starting this observer. It does not change the shipping image, display-manager configuration or user settings.

The root observer reads complete PSS/RSS and fd counts for the session UID. It requires exactly one live compositor and GTK shell, records their PID/start-time identities (the legacy shell host is an alternative renderer, not a concurrent process), and fails if any exits or restarts. Each sample makes bounded calls on the user's real bus to the screen saver, calendar server, portal settings and GNOME portal backend. Missing counters and unreadable live processes fail instead of counting as zero. Departed noncritical processes and zombies are excluded; process identity changes during sampling fail.

After a ten-minute warmup, every sample must remain below the baseline PSS multiplied by 1.5 plus 16 MiB, the baseline fd count multiplied by 1.5 plus 32, and the baseline process count plus 12. These explicit endurance limits detect large growth; they are not a performance parity threshold. The baseline includes that UID's activated services and applications, so use a controlled, unchanged workload. The optional native workload below adds actual frame pacing. The observer cannot measure compositor object counts, GPU memory, input latency or service fault recovery. Issues #203 and #73 remain open for the remaining requirements and for the completed actual 24-hour report.

Copy the script to the VM and run it as a root transient service, supplying the UID's account and the exact source revision from the deployment record:

```sh
sudo systemd-run --unit=roost-soak --property=Type=exec \
  /usr/local/libexec/roost-vm-soak --user roost-test \
  --source-revision EXACT_INSTALLED_REVISION --out /var/tmp/roost-soak-NEW_RUN
```

Each output directory must be fresh. `provenance.json` contains bootc status, boot ID, actual installed binary hashes, supplied source revision, requested duration and growth limits. `samples.jsonl` grows after each successful sample. `report.json` is written on normal completion or a handled failure. A successful report requires `complete: true` and the full requested elapsed duration. For qualification require `requested_duration_s: 86400`; shorter rehearsal runs are not 24-hour evidence. Interrupting the observer or rebooting the VM cannot qualify a run.

The observer must remain outside the session UID so its memory and helper processes are excluded. Root/GDM services, kernel allocations and GPU buffers are outside the reported scope. It performs health calls; the optional native workload adds one controlled mapped window, without input or window churn; pair its report with the separate nested stress evidence. Changing the installed payload, restarting a critical process or switching images requires a fresh run and output directory. Collect the completed report, provenance, raw samples and scoped session journal together; review journal crashes and the separately measured frame pacing before calling the roadmap release gate complete.

The pure failure fixtures in `scripts/lib/roost-soak-tests.py` verify zombie rejection, critical-process loss, missing counters, PID reuse, unreadable live processes and parenthesized process names. They validate the observer, and do not establish a guest endurance result.

## Native frame pacing during endurance

For a memory-and-frame-pacing run, build the small fixture with
`scripts/roost-frame-pacer-build`, install its binary as the root-owned
`/usr/local/libexec/roost-frame-pacer`, and copy
`scripts/lib/endurance_presentation.py` to
`/usr/local/libexec/lib/endurance_presentation.py` alongside the observer.
Use the running user's actual ordinary Wayland socket, for example:

```sh
sudo systemd-run --unit=roost-soak --property=Type=exec \
  /usr/local/libexec/roost-vm-soak --user roost-soak \
  --source-revision EXACT_INSTALLED_REVISION --out /var/tmp/roost-soak-NEW_RUN \
  --presentation --wayland-display wayland-0
```

The 320×180 mapped client alternates two SHM buffers through ordinary frame
callbacks and retains real `wp_presentation` feedback. It requests an ordinary
idle inhibitor while mapped, like a video player; it never unlocks the session
or overrides an explicit lock. Keep the probe visible and the workload
controlled. An explicit lock, a hidden/stalled probe, lost connection or two
seconds without presentation after startup fails this active workload. Lock
and suspend security qualification remains the separate actual VM journey.

`presentation.jsonl` retains bounded batches of at most 240 frames, followed
by an explicit completion record. Every memory sample includes frame interval
p50/p95/p99 and counts for feedback received since the previous sample,
including intervals crossing batch boundaries. Counters reconcile presented,
discarded and pending commits; unknown refresh and absent hardware flags
remain explicit. The observer checks monotonic timestamps, commit identity,
clock stability, complete batch order and requested elapsed duration. Capture
output is limited to 4 GiB and record size to 128 KiB. Truncated or missing
completion cannot qualify a soak.

The probe runs as the observed ordinary UID, so its memory and descriptors
are part of the controlled active-workload baseline. Keep this distinction
when comparing with a memory-only idle soak. These intervals describe this
native client workload, not input latency or all compositor animation frames.
No GNOME-relative release threshold is introduced. A short DRM control
exercises batch rollover and completion in CI; it is not 24-hour evidence.
