---
created_date: "2026-09-27"
document_status: draft
---

# Program baseline and technical spikes

**Status:** Draft  
**Parent:** [Program architecture](../../docs/architecture.md)  
**Role:** Establish evidence and unblock the implementation speks; no production desktop behavior is delivered.

## Overview

Before committing to a UI toolkit, hardware support claim, or performance target, create a reproducible GNOME comparison baseline, prototype the critical shell UI/accessibility paths, and record the project threat model and protocol gaps. This work determines the constraints for the nested vertical slice and daily-driver speks without turning an early prototype into an architectural commitment.

## Requirements

- **R1. Baseline identity:** select and record a pinned GNOME release, distro/image, kernel, GPU/driver, monitor topology, display settings, services, extensions, applications, and workload. Comparison runs use the same machine and equivalent settings.
- **R2. Measurement recipe:** publish scripts and methods for startup, idle CPU, whole-session PSS, frame-time distribution, missed deadlines, input-to-photon latency where measurable, GPU memory, and soak growth. Preserve raw traces with privacy-safe redaction.
- **R3. Toolkit spike:** compare GTK4/libadwaita with at least one plausible Rust-native candidate on an overview, panel/layer surface, quick-settings interaction, large-text/reduced-motion behavior, keyboard navigation, AT-SPI screen-reader flow, IME, RTL, and memory/frame measurements.
- **R4. Smithay/protocol spike:** run a nested compositor prototype, map representative GTK/Qt clients, inventory protocol implementation gaps, and pin candidate Smithay/protocol versions for the prototype.
- **R5. Threat model:** identify assets, actors, same-UID process assumptions, trusted shell/lock/portal roles, capture/remote-input authorities, extension risks, crash boundaries, and security review requirements.
- **R6. Decision records:** record evidence-backed decisions or explicit unresolved questions for baseline, UI toolkit, nested backend, process identity, and project support targets.
- **R7. No premature claims:** benchmark and spike results are engineering evidence, not claims that the full desktop is faster, safer, more accessible, or more compatible than GNOME.

## Constraints

- Compare equivalent workloads on the same hardware and current pinned GNOME behavior; old bug reports are research leads, not evidence of present defects.
- Do not select a toolkit based only on programming language, memory intuition, or screenshot similarity.
- The nested prototype is not a production security or performance result.
- Do not introduce arbitrary real-time priorities or extra rendering threads as part of a spike without measured need.

## Acceptance criteria

- **A1. Reproducible baseline:** a second developer can reproduce a GNOME run from the documented environment/workload recipe and obtain the expected trace set.
- **A2. Comparable results:** baseline traces include repeat count, variance/noise notes, configuration, and raw artifact locations; no target threshold is asserted until a baseline has been measured.
- **A3. Accessibility evidence:** toolkit comparison includes working keyboard and screen-reader journeys across the prototype surface; gaps and remediation cost are recorded.
- **A4. Protocol gap list:** nested prototype and real clients yield a versioned support/gap matrix covering app mapping, input, shell surfaces, scaling, frame/capture, lock, and XWayland paths.
- **A5. Threat model review:** trust boundaries, same-UID assumptions, and fail-closed conditions are reviewed before any production privileged API is approved.
- **A6. ADRs:** each decision is either resolved with evidence or marked open with owner, consequence, and the latest decision date needed by dependent work.

## Dependencies

No implementation spek dependency. Findings constrain specs 001–006. Hardware resources and a chosen GNOME baseline are required for final baseline completion; the nested vertical slice may proceed against documented provisional assumptions while those are pending.

## Risks and open questions

- The user-visible baseline release, target distro, and reference GPU/monitor are not yet selected.
- Screen-reader/toolkit testing may need an assistive-technology participant and supported hardware.
- Performance measurements need external latency equipment for strong input-to-photon claims; software timestamps are a qualified proxy.
