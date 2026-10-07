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
