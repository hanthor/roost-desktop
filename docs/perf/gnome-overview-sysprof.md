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

These files contain GNOME scopes, **not a derived latency measurement**.
Scope names can be truncated to 39 bytes in the capture format. Event processing,
frame dispatch and presentation must be associated by frame ownership before
calculating handling-to-presentation latency; a frame queued before an input
cannot count as its response. GNOME's `presentation was N µs earlier` description
also uses a timestamp taken inside a scope, so recovering it from a scope boundary
requires explicit bounds. QMP screenshot polling remains a separate host-visible
response bound. No parity threshold is introduced here.

Primary contracts:

- [Sysprof 51 profiler D-Bus interface](https://github.com/GNOME/sysprof/blob/51.0/src/sysprofd/org.gnome.Sysprof3.Profiler.xml)
- [Sysprof 51 capture ABI](https://github.com/GNOME/sysprof/blob/51.0/src/libsysprof-capture/sysprof-capture-types.h)
- [Mutter 51 profiler](https://github.com/GNOME/mutter/blob/51.0/src/core/meta-profiler.c)
- [Mutter 51 frame-clock traces](https://github.com/GNOME/mutter/blob/51.0/clutter/clutter/clutter-frame-clock.c)
