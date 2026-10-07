# Stock GNOME SVG backgrounds

The isolated `svg-run.sh` session uses the actual packaged compositor, GTK
shell and separately packaged `roost-wallpaper-svg` binary. The helper uses
the installed GdkPixbuf SVG stream loader, which delegates to librsvg and
native FontConfig/Pango fonts without assigning a base URI. Embedded data and
internal references retain the loader's semantics. Ordinary local/remote
external references are governed by that actual stream loader, rather than a
hand-written substitute.

The helper has a 64 MiB input bound, 33,177,600 intrinsic pixel bound, 1 GiB
address-space bound, 4-second CPU soft/5-second hard limit, and the parent
kills/reaps it after 5 seconds wall time. It never silently resizes an
oversized source. Every transport operation uses nonblocking pipes on the
existing bounded worker. All filesystem, font/library initialization and
helper waits happen outside compositor frame callbacks. Cache identity
includes the worker-observed hash of the actual helper, mapped libraries,
FontConfig configuration and configured font files. Old worker completions
remain counted and cannot satisfy a changed identity/geometry key.

The runtime proof retains actual loader/font/helper file hashes and RPM
ownership/version/verification. It selects the real SVG item and light/dark
controls in unmodified GNOME Settings 51 under ordinary UID 1000, verifies
shared persisted desktop/lock/style keys, closes Settings, compares retained
desktop screenshots to an independent actual installed GdkPixbuf loader, and
checks CSS, viewBox, internal use, transparency, native text and embedded PNG
pixels. It rewrites the same SVG URI and verifies new pixels, checks honest
fallback for malformed/excessive intrinsic dimensions, restores the valid
source, observes actual lock pixels, and genuinely restarts the compositor
and GTK shell before verifying persisted dark SVG pixels again.

Until this exact head's runtime checks pass, this is pending qualification.
Even a successful isolated 1280×800 scale-1 session does not close issue #452:
physical DRM outputs and scales, the booted final shipping image, broader SVG
reference/font/resource limits and loader variants remain explicit full-scope
acceptance work. Dynamic XML issue #360 and the full GNOME Settings audit
remain separate open acceptance scopes.
