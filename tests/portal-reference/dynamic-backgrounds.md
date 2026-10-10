# GNOME51 XML background qualification

This extends the static background batch. The stock GNOME51 Background chooser
selects an actual wallpaper-list item with light/dark XML descriptors. Real
Default/Dark accessibility actions change the persisted color scheme. The app
identity, process owner and executable guards, ordinary UID1000, shared keyfile
settings, and unobstructed desktop capture remain required.

The XML timeline uses actual local-calendar starttime and actual wall clock;
no replacement timestamp is supplied to the compositor or reference library.
The installed GNOME51 GnomeBG4 BGSlideShow independently queries each observed
phase. Retained before/after wall timestamps, reference paths/progress/duration,
actual RGB pixels, screenshots, and repeated static phases across the natural
24-second cycle qualify both static frames, both central crossfades and wrap.
The actual compositor state retains the selected paint generation, root/selected
source identities, geometry, sample wall timestamp, sample progress and cadence.
The proof brackets each independent GNOME reference query and actual screenshot
with real wall timestamps, requires unchanged paint metadata across capture, and
derives the reference progress interval at the candidate's actual sample time.
Progress must match that interval within0.002 and pixels within2 RGB units.
Sample age must stay within its GNOME cadence plus0.5seconds; screenshot capture
itself must take at most0.5seconds. The compositor also stops retaining an old
completed same-epoch frame beyond that age bound. Changed source/calendar/geometry
epochs cannot retain old completed pixels. A sample cannot pass from elapsed
time alone or from an enlarged RGB tolerance.

The same actual image URI changes bytes and must change its static pixels.
The same XML URI swaps its timeline paths and must produce the new reference
phases. DTD/entity and oversized XML fixtures must fall back to the configured
solid color. A genuine candidate compositor and GTK-shell restart must retain
XML/style settings and resume the natural dark timeline before reopening Settings.

The renderer places each frame independently, blends premultiplied contributions,
and adds the opaque pattern beneath the resulting coverage. Referenced images,
XML parsing, source identity checks and local timezone conversion run only on
bounded workers. Frame code uses immutable snapshots and actual wall-time samples.
Running obsolete workers remain counted until completion. XML/resource and image
limits apply before parsing/decoding. Decoded images have4 slots; independent
placement planes have4 slots and a64MiB total byte budget. Larger planes compose
on the bounded worker without that optional placement cache. Dynamic final frames
never create per-step disk cache entries. Full geometry/source/XML epoch/progress
keys reject stale results.

XML contains bare filesystem paths, as GNOME does. Only the root settings URI is
decoded once. Non-.xml SVG markup is not classified as a successful slideshow;
SVG image support remains the separate #452 gap.

The pure Rust cases cover exact wall-time boundaries, cycles before starttime,
variant tie order and prefer-large aspect selection, parser limits, independently
placed transparency and cached composition equivalence. This narrow runtime
fixture uses one1280x800 nested output. It does not qualify physical multi-output
variant changes, actual suspend/timezone changes, final published images, or
complete #360. Those remain open until their actual runtime evidence passes.
