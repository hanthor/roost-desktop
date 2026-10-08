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
Tuna Desktop's accepted-action point trace must preserve this difference and use the
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


## Explicit stock and diagnostic benchmark profiles

The benchmark prepares one coherent distribution GNOME 51 image, records its
image ID, and derives two explicit references from those same bytes. `stock`
uses the distribution's unmodified Mutter package before diagnostic replacement;
this means distribution stock, not an upstream build without distribution
patches. `diagnostic` installs the exact checksum-pinned `mutter 51.0-1.6`.
The default profile and every original diagnostic Start/Stop, writer-closure,
whole-capture, source and ownership gate remain required.

Each profile installs the same authenticated trusted-main Tuna Desktop payload into
one shared preview image. Its GNOME and Tuna Desktop sessions use that profile's image,
package manifest and critical kernel/library hashes. Stock admission verifies
original ordinary GNOME session UID, unique bus owner, PID, start ticks and
installed executable, plus actual loaded Mutter/Clutter/Cogl inode identities,
package ownership and hashes against the protected pre-instrumentation image.
These exact receipts are retained before and after the workload. An original
process replacement or changed reference fails the acquisition.

Stock runs the same resource, QMP overview, native presentation-cadence and
notification workloads. Its trace result is explicitly unavailable: stock does
not request diagnostic Sysprof options or substitute zero latency. A diagnostic
failure never falls back to stock. Both profiles retain raw samples, guest-clock
phase markers, captures and actual image/package receipts.

Each profile performs three fresh-guest pairs on one host, ordered GNOME/Tuna Desktop,
Tuna Desktop/GNOME, GNOME/Tuna Desktop. Reports retain actual repeat and order; the order count
is 2:1, not fully balanced or randomized. First diagnostic paths are unchanged;
later pairs use `repeat-2/` and `repeat-3/`, and stock pairs live beneath `stock/`.
All twelve acquisitions are mandatory for whole-workflow success. Each profile
fails at its original first failed command, retaining that exit status and a
finite `acquisition-status.json` stage/case receipt. A failed diagnostic step
still permits the independent stock step on the same runner only after the
shared package, both reference images, and all installed-runtime guards passed.
Before any stock image or disk operation, its preflight also requires the
original finite diagnostic status and every reached case's original owned-VM
departure receipt. Each worker creates a private original FD before boot, pins
its returned child PID/UID/start time and pidfd, and records terminal departure
only after actual wait and pidfd readiness. Transport-close errors still run
owned-child cleanup; the original metric failure remains nonzero. Missing,
changed or nonterminal receipts (including an abruptly killed worker) forbid
stock acquisition. No process-name scan or unrelated process killing is used.
These same-user artifact checks establish controlled source provenance rather
than an unforgeable security boundary. A departure receipt proves neither guest
measurement success nor package/runtime qualification. Cancellation or failed
shared setup forbids stock acquisition. Neither profile retries its failed case.
Diagnostic failure keeps the workflow failed, and partial diagnostic reports
must not be pooled or labeled qualified. Successful stock pairs are separately
scoped evidence, never a fallback diagnostic result. The workflow uploads
retained evidence, without retries or selecting only successful samples.
The job has a finite 180-minute budget, extended from the original single-pair
lane to cover twelve fresh installations/acquisitions and one diagnostic build.
This source budget has not yet been qualified by the new VM run.

Idle CPU uses only intervals wholly within the actual guest idle window. Idle
PSS and RSS use only collections wholly inside that window and require at least
twenty samples. Whole-run summaries include startup and later workloads. PSS
and RSS sum the test UID's processes, excluding the observer and descendants;
RSS can double-count shared pages. Host polling of the panel includes VM boot
and provides an upper bound, not exact session startup. QMP overview timings
are broad visible host responses, not input-to-photon. Search followed by an
eight-second sleep neither measures nor proves app launch/readiness.

Three pairs do not establish statistical significance or a release threshold.
Host cache warming/contention, virtual GPU behavior and diagnostic mark/drain
overhead remain confounds. Physical cold/warm startup and sustained app/soak
measurements remain open.

## Retained single-pair observations before the stock lane

PR #461's successful comparison used instrumented GNOME 51, not stock. Its
trusted-main measured Tuna Desktop payload was `e970c7b12a66a2c338ee8e8c5ab01d2ccd748bfc`;
the benchmark observer was the PR merge source. Linux was `7.2.9.arch1-1`, Mesa
`1:26.2.4-1`, GNOME Shell `1:51.0-1`, and Mutter `51.0-1.6`.
The original artifact is from run `37620809723`, job `112790314887`.

| Observation | Instrumented GNOME | Tuna Desktop |
| --- | ---: | ---: |
| Panel observed upper bound | 28.7368 s | 24.5626 s |
| Whole-user PSS p50 | 927766 KiB | 929079 KiB |
| Idle sampled CPU mean | 0.206869% | 1.103330% |
| Whole-run sampled CPU mean | 21.330033% | 41.082553% |
| Overview host upper bound p50 | 0.165681 s | 0.452012 s |
| Overview host upper bound p95 | 0.534667 s | 0.760166 s |
| Native client presentation interval p95 | 26.669 ms | 13.335 ms |

These mixed single-pair observations demonstrate no repeatable stock-GNOME
benefit. CPU percentages use one core as 100% and count surviving sampled
processes; departed processes can consume unobserved CPU. The presentation
probe is one native SHM client on the virtual display, not all compositor frames.
New stock/repeat results must carry their own successful qualification.
