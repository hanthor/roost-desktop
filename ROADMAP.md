# Roost roadmap: October 2026–October 2027

Updated 2026-10-04. This is the public contribution roadmap; [program gates and requirement traceability](docs/roadmap.md) describe the acceptance process. Work and evidence live on the [roadmap board](https://github.com/users/hanthor/projects/4). Dates are planning windows, and release qualification depends on evidence.

## Current status

Roost has a nested developer preview and an actual Marlin VM test lane. The daily-driver candidate remains open. The target is a Roost flavor of TunaOS Marlin, with GNOME 51 as its comparison baseline. The published Marlin GNOME image measured on 2026-10-04 still contains GNOME Shell 50.5; GNOME 51 comparisons require a separately recorded, fully upgraded reference payload. A build or merged feature alone does not establish parity or release readiness.

The latest GitHub release entry is [v0.0.0-ci, “Demo media bundle”](https://github.com/hanthor/roost-desktop/releases/tag/v0.0.0-ci), published 2026-10-01. It contains screenshots and videos, not an installable desktop release. The six session binaries use development package version 0.1.0. The [README](README.md) now shows real screenshots and an overview recording.

At this update, GitHub reports three contributor entries, zero stars and zero forks. The media release assets have one download in total. Media downloads are not package installs, and none of these counts measures active users. Package-download and installed-user metrics are unavailable; Roost does not collect usage telemetry. These dated counts are a starting record, not an adoption claim.

## Next three months: through 2027-01-04

1. Finish and validate the core shell journeys: workspace insertion, app-grid hover/page behavior, notifications, shortcut consent, IME caret placement and live settings interoperability. Keep concrete GNOME differences in the [parity ledger](docs/gnome-parity.md), with actual graphical acceptance evidence.
2. Complete secure screen sharing and RemoteDesktop integration ([#61](https://github.com/hanthor/roost-desktop/issues/61)). Require real GNOME portal consent, authenticated ownership, disconnect and lock revocation, and verified input delivery for each advertised capability.
3. Qualify one signed amd64 Marlin Roost image ([#69](https://github.com/hanthor/roost-desktop/issues/69)): current-source package, clean installation, graphical login, portals, PAM, boot and update/rollback evidence. This scope adds no ISO or LUKS matrix cells.
4. Run the actual 24-hour endurance and comparable performance work ([#203](https://github.com/hanthor/roost-desktop/issues/203), [#73](https://github.com/hanthor/roost-desktop/issues/73)). Record binary/image provenance, resource growth, frame pacing and failures. Investigate measured regressions before making performance parity claims.
5. Record hardware and session qualification ([#62](https://github.com/hanthor/roost-desktop/issues/62), [#68](https://github.com/hanthor/roost-desktop/issues/68)): physical GPU/driver combinations, mixed displays, VT switching, suspend/resume, logout and fail-closed lock behavior. AWS KubeVirt VM evidence covers its own profile only.

## Six to twelve months: 2027-04-04–2027-10-04

The next delivery stage is a daily-driver candidate after the security, usability, hardware, recovery and performance gates pass. Architectural work should reduce unnecessary repainting, complete protocol/application compatibility, improve fault recovery and preserve the bounded compositor critical path. A faster benchmark must retain the same workload and measurement scope.

Ecosystem work centers on GTK, Qt and XWayland application journeys; accessible keyboard and screen-reader use; live GNOME settings compatibility; and reliable existing system services. Broader hardware and distribution support should follow a named, reproducible support matrix rather than an untested compatibility claim.

Adoption work should make the qualified amd64 image and its install/recovery instructions usable by external testers, collect reproducible issue reports, and publish the supported configurations and known gaps. Report package download and contributor counts when the publication tools supply them. No numerical active-user target is set without a way to measure it; collect feedback voluntarily and preserve the no-telemetry default.

A 1.0 release requires the [global decision gates](docs/roadmap.md#global-decision-gates). This roadmap does not assign a release date before those gates pass.

## Known limitations and scope

Screen-reader speech, physical GPU/VT/suspend behavior, GPU memory/frame pacing and completed 24-hour endurance are not established by nested graphical tests. Settings support is key-specific; see the [compatibility map](docs/settings-map.md). Modern RemoteDesktop features must remain unadvertised until implemented and tested. Exact visual parity requires paired captures from the declared reference version.

Public extension APIs remain optional later work, outside the initial core release unless scope changes through the recorded decision process. In-process shell extensions, an unbounded plugin authority model and unsupported platform claims are outside this roadmap. No shipped feature deprecation is scheduled; protocol version changes must preserve the documented compatibility contract or explicitly reject incompatible peers.

## Where contributors can help

Useful contributions include reproducible GNOME 51 comparisons, accessibility and language journeys, hardware support reports with exact kernel/GPU/driver/topology, protocol and service failure regressions, comparable performance traces, and packaging/recovery verification. Start from an open [roadmap issue](https://github.com/users/hanthor/projects/4) and identify the acceptance evidence your change supplies.

Use the existing [test strategy](docs/test-strategy.md) and report real runtime failures. Keep unsupported capability advertising, new image/ISO matrix expansion and public extension work out of unrelated fixes. Source tests, screenshots and a successful boot each establish different parts of the release evidence; retain their limits when updating an issue.
