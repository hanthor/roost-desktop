# GNOME overview Sysprof capture

The paired GNOME 51 VM records the installed GNOME Shell's own Sysprof scopes
during the twenty actual overview inputs. This is CI instrumentation; the shipping
desktop configuration does not enable a profiler.

The fixed guest helper authenticates the unique `org.gnome.Shell` owner using
D-Bus UID/PID credentials and `/proc/PID/exe`, then calls
`org.gnome.Sysprof3.Profiler.Start(a{sv}, h)` at
`/org/gnome/Sysprof3/Profiler` with an exclusively created 0600 capture file.
It calls Stop only after its own successful Start and refuses an owner change.
Failures during the workload attempt to stop that owned profile before VM teardown.

The host retains `gnome-overview.syscap`, its peer/timestamp/size/SHA-256 provenance,
and a structural decode in `gnome-overview-marks.json`. Transfer uses bounded
64 KiB chunks, checks their total size and digest, and rejects captures larger
than 8 MiB. Truncated frames, malformed mark strings and negative durations fail
the decoder. Unknown frame types are structurally validated and remain in the
raw capture. The decoder supports the little-endian version-1 ABI used by the
amd64 baseline. A fixture generated with the actual Sysprof writer validates
the layout independently of the Python decoder.

The host also retains `gnome-overview-frame-ownership.json`. This analysis is
restricted to the pinned GNOME Shell 51.0, authenticated Shell PID, single
`Virtual-1` output and isolated twenty-Super-tap workload. The source fixture
already requires GNOME 51 and injects only these inputs during this capture.
It requires exactly twenty release-handler scopes; autorepeated presses do not
become additional actions.

The decoder follows Mutter 51's actual frame ownership transitions:
`dispatch()` fills `next_presentation`, then `next_next_presentation`;
`notify_presented()` completes the first slot and advances the second;
`notify_ready()` aborts the newest slot. It requires exactly one nested
`swap_framebuffer()` for each presented owner, no swap for an aborted owner,
and no pending slots at the end. A completed frame dispatched before an input
cannot become its response. Missing events, two-slot overflow, ambiguous swaps,
unexpected outputs, overlapping input/dispatch scopes and responses crossing
another input fail the measurement.

For each release handler, the response is the first successfully presented
owned frame whose dispatch starts after the handler completes. The actual
accepted-toggle instant lies somewhere within the handler; these scopes do not
identify that instant. The analysis therefore retains lower and upper latency
bounds, with separate p50/p95/p99 and counts, rather than an exact point value.
GNOME's `presentation was N µs earlier` description samples monotonic time
inside its presented scope. Recover its kernel timestamp as an interval across
that scope, with explicit microsecond rounding room. No host/guest clock
subtraction is involved. QMP image-response bounds remain separate observations.

The separately reported KMS-ready time is userspace feedback, not the kernel
flip timestamp. Mutter copies it into frame info and emits it in the presented
notification; it can follow the kernel flip. Validate it within the owned
frame's dispatch-to-notification lifetime, rather than using the reconstructed
kernel timestamp as its upper bound. This does not change presentation bounds
or admit missing owners, swaps, completions or pending end-of-capture slots.


The raw Sysprof capture remains authoritative. An unmodified subset of the
actual paired capture retains all relevant native mark frames with source
revision, package versions, full-capture and subset hashes. That fixture balances
354 dispatches, 337 presentations and 17 aborted frames, with twenty distinct
input responses. Regression mutations remove or duplicate completions, change
outputs, remove swaps, add inputs and create overlap; each must fail.

This is controlled event-handler-to-first-later-frame timing, not device-arrival
latency or a claim about the precise visible animation stage. Comparison with
Roost's accepted-action point trace must preserve this difference and use the
GNOME intervals explicitly. No parity threshold is introduced here.

Primary contracts:

- [Sysprof 51 profiler D-Bus interface](https://github.com/GNOME/sysprof/blob/51.0/src/sysprofd/org.gnome.Sysprof3.Profiler.xml)
- [Sysprof 51 capture ABI](https://github.com/GNOME/sysprof/blob/51.0/src/libsysprof-capture/sysprof-capture-types.h)
- [Mutter 51 profiler](https://github.com/GNOME/mutter/blob/51.0/src/core/meta-profiler.c)
- [Mutter 51 frame-clock traces](https://github.com/GNOME/mutter/blob/51.0/clutter/clutter/clutter-frame-clock.c)


The CI reference variant `mutter 51.0-1.1` adds a trace-context lifetime correction
to the pinned Arch 51.0 source: each thread context releases the reference it
acquired during construction. This permits the final context release to flush
and close the Sysprof writer. The original Arch recipe hash, changed recipe,
exact patch and resulting package hash are retained in the comparison artifacts.
Build tools remain in a separate image stage. Reports using this variant must
identify it explicitly; previous captures from `51.0-1` remain separate evidence.

The observer retains the actual mapped Cogl library identity and checks it across
Start/Stop. After Stop, acquisition waits at most five seconds for the original
GNOME process to close its writable capture descriptors. This is a condition
check, not acceptance based on elapsed time or file quietness. Raw bytes and
decoded marks are retained even when the writer does not close; such a capture
cannot qualify. Actual closure still does not bypass whole-file integrity,
required scopes, strict frame ownership, repeated measurements or release gates.

## Rejected presentation evidence

A complete capture with closed writer descriptors can still fail timing
qualification. The second GNOME capture in job 112576389653 (run 37554162135,
artifact 11455021454) reconstructed a presentation at
123677782000–123677789000 ns for a dispatch beginning at 123682891000 ns.
The strict qualifier rejects this frame; it cannot become a latency sample.

For this failure, `gnome-overview-frame-rejection.json` retains the reason,
whole-capture digest, original dispatch and notification marks, reconstructed
presentation bounds, KMS feedback readiness, swap count, and remaining pending
dispatches. The whole raw capture, source provenance and decoded marks remain
authoritative; no qualified ownership result is emitted. These scope-order
associations are not independent source frame identifiers or raw kernel flip
events, so the report does not establish whether the cause is ownership or the
presentation provider. Issue #434 tracks that unresolved distinction.

The gzip fixture `gnome51-presentation-before-dispatch.syscap.gz` contains the
entire unmodified rejected capture, with its original provenance and artifact
coordinates in the adjacent JSON. Tests verify the whole checksum, continued
strict rejection, exact observed bounds, and preservation of rejection evidence
without producing a qualified result.


## Instrumented source-frame diagnostic variant

The reference package `mutter 51.0-1.2` additionally applies
`frame-source-evidence.patch` to the pinned 51.0 source. It records actual
Clutter dispatch counters, the presentation's view/global-frame counters,
supplied presentation time, sequence, flags and KMS readiness, and the raw
atomic KMS callback's CRTC, sequence, seconds,
microseconds and device path. Original scheduling, timestamp handling and the
strict ownership qualifier remain unchanged. This variant is diagnostic: extra
trace marks have measurement overhead, and these measurements are not final
performance parity evidence.

`gnome-overview-source-frames.json` retains these independently observed values
before timing qualification. Missing, duplicate, foreign-PID or malformed
source records fail validation; early presentation times are retained exactly,
not repaired. The original complete capture/provenance remains authoritative.
No pairing between a kernel event and a view-frame counter is inferred merely
because their times are close. A rejected ownership result remains rejected.
The recipe, both patches and package checksum manifest are retained with each
run, and the original mapped library's installed package identity selects this
additional diagnostic requirement.


## Three fresh-guest acquisitions per desktop

The repeatability lane runs three complete GNOME/Roost pairs on the same host
and shared image/package baseline. Each desktop uses a newly installed guest
disk. Pair one keeps the original artifact paths; subsequent pairs use
`repeat-2/` and `repeat-3/`. Each report records `acquisition_repeat`,
`acquisition_order=gnome-first-fixed`, and
`reference_kind=instrumented-gnome51`.

Every acquisition must pass the original writer-closure, whole-capture integrity,
actual Start/Stop boundary, source identity, and strict ownership checks. A failure
stops the workflow and uploads the evidence already retained. It does not rerun a
rejected capture, replace timestamps, trim frames or select only successful pairs.
These are fresh-guest repeatability measurements, not physical cold-host or warm
session measurements. The trusted original main package and pinned shared image
remain explicit; no newer feature candidate is inferred from the observer branch.

GNOME runs first in every pair. Fixed order, shared host contention and warmed
host/container caches can confound a comparison. Three pairs do not resolve
those effects or establish a release threshold. The current reference uses
instrumented `mutter 51.0-1.6`, including diagnostic marks and bounded drained
capture gates; its overhead is not measured here. Results must be labeled as
comparisons with an instrumented GNOME 51 reference, not an unmodified stock
GNOME benchmark. Stock-reference resource/cadence measurements require a
separately identified unmodified lane; unavailable stock frame-ownership timing
must remain unavailable rather than become zero.
