# Tuna Desktop roadmap: October 2026–October 2027

Updated 2026-10-08. Tuna Desktop was previously named Roost; code, packages and older evidence still use that name until the rename lands ([#504](https://github.com/tuna-os/tuna-desktop/issues/504), [#505](https://github.com/tuna-os/tuna-desktop/issues/505)). This is the public contribution roadmap; [program gates and requirement traceability](docs/roadmap.md) describe the acceptance process. Work and evidence live on the [roadmap board](https://github.com/users/hanthor/projects/4). Dates are planning windows, and release qualification depends on evidence.

## Current status

Tuna Desktop has a nested developer preview, an actual Marlin VM test lane, and a signed experimental amd64 Marlin Tuna Desktop image. The daily-driver candidate remains open. TunaOS merged the flavor, then named Roost, in [PR #2991](https://github.com/tuna-os/tunaOS/pull/2991). Its qualified image passed an actual AWS KubeVirt bootc switch, shipped greeter selection and PAM login ([retained evidence](docs/marlin/2026-10-04-flavor-acceptance/README.md)); the installed `0.1.0-2` package is pinned to Tuna Desktop `c28f96eb` and needs a refresh with later fixes before endurance qualification. GNOME 51 remains the comparison baseline. The published Marlin GNOME image measured on 2026-10-04 still contains GNOME Shell 50.5; GNOME 51 comparisons require a separately recorded, fully upgraded reference payload. A build or merged feature alone does not establish parity or release readiness.

The latest GitHub release entry is [v0.0.0-ci, “Demo media bundle”](https://github.com/hanthor/roost-desktop/releases/tag/v0.0.0-ci), published 2026-10-01. It contains screenshots and videos, not an installable desktop release. The six session binaries use development package version 0.1.0. The [README](README.md) now shows real screenshots and an overview recording.

At this update, GitHub reports three contributor entries, zero stars and zero forks. The media release assets have one download in total. Media downloads are not package installs, and none of these counts measures active users. Package-download and installed-user metrics are unavailable; Tuna Desktop does not collect usage telemetry. These dated counts are a starting record, not an adoption claim.

## Direction: GNOME's functionality, better performance

Tuna Desktop aims to do everything GNOME 51 does and to run lighter than GNOME on the same machine. Parity at rest is close: most screens are within 0.1–1.7% of GNOME 51's pixels. Performance is not there yet. On the 2026-10-04 paired Marlin VM run, memory matched GNOME, but median CPU was 159% of a core against GNOME's 19%, and the median overview response was 382 ms against 159 ms. Closing and reversing that gap is a release requirement, tracked in [#503](https://github.com/tuna-os/tuna-desktop/issues/503).

## Next three months: through 2027-01-04

1. Finish and validate the core shell journeys: workspace insertion, app-grid hover/page behavior, notifications, shortcut consent, IME caret placement and live settings interoperability. Keep concrete GNOME differences in the [parity ledger](docs/parity-ledger.md) and pixel comparisons in [GNOME parity captures](docs/gnome-parity.md), with actual graphical acceptance evidence.
2. Complete secure screen sharing and RemoteDesktop integration ([#61](https://github.com/hanthor/roost-desktop/issues/61)). Require real GNOME portal consent, authenticated ownership, disconnect and lock revocation, and verified input delivery for each advertised capability.
3. Refresh and qualify the admitted signed amd64 Marlin Tuna Desktop image: current-source package, clean installation, graphical login, portals, PAM, boot and update/rollback evidence. Initial flavor admission and boot/login acceptance are complete ([#69](https://github.com/hanthor/roost-desktop/issues/69)); later source fixes and endurance qualification remain open. This scope adds no ISO or LUKS matrix cells.
4. Run the actual 24-hour endurance and comparable performance work ([#203](https://github.com/hanthor/roost-desktop/issues/203), [#73](https://github.com/hanthor/roost-desktop/issues/73)). Record binary/image provenance, resource growth, frame pacing and failures. Investigate measured regressions before making performance parity claims.
5. Record session lifecycle qualification ([#62](https://github.com/hanthor/roost-desktop/issues/62), [#68](https://github.com/hanthor/roost-desktop/issues/68)): real VM suspend/resume, VT switching, logout and fail-closed lock behavior, plus the separate physical GPU/driver and mixed-display support matrix. AWS KubeVirt VM evidence covers its own profile only.
6. Port every GNOME 51 animation ([epic #492](https://github.com/tuna-os/tuna-desktop/issues/492), [spec](.spektacular/specs/20261008122728-6ac4a932-gnome-animation-parity.md)). An audit on 2026-10-08 found 6 of GNOME 51's 54 animations ported and 7 partial. Deliver, in order: GNOME's motion policy (reduced motion keeps fades, slow-down factor) and a deterministic test clock; window open and close; minimize; maximize and tiling; workspace slide and swipe; modal dimming; overview corrections; app grid; menus and transient surfaces; lock screen and desktop transitions. Each family needs a CI gate and a paired GNOME 51 capture, and must cost less CPU than GNOME.
7. Beat GNOME 51 on CPU, frame pacing and overview response on the same Marlin VM ([#503](https://github.com/tuna-os/tuna-desktop/issues/503)). Profile first, then set regression gates ([#315](https://github.com/tuna-os/tuna-desktop/issues/315)).
8. Finish the rename to Tuna Desktop and bring the docs in line with the code: user-facing names ([#504](https://github.com/tuna-os/tuna-desktop/issues/504)), a coordinated internal and package rename ([#505](https://github.com/tuna-os/tuna-desktop/issues/505)), current screenshots ([#506](https://github.com/tuna-os/tuna-desktop/issues/506)) and complete parity-ledger rows for every open GNOME 51 gap ([#507](https://github.com/tuna-os/tuna-desktop/issues/507)).

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
