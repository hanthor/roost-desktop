---
created_date: "2026-09-27"
document_status: draft
---

# Hardware session and application compatibility

**Status:** Draft  
**Parent:** [Program architecture](../../docs/architecture.md)  
**Depends on:** 000 program baseline and technical spikes; 001 nested compositor and shell recovery. Shell parity from 002 informs end-to-end testing.

## Overview

Move from nested development sessions to a documented hardware session and establish common application/peripheral compatibility. Integrate DRM/KMS through logind/libseat, output hotplug, XWayland, scale, clipboard, drag-and-drop, IME, device input, and suspend/resume with capability-based fallbacks. This spec owns display/backend and application protocol compatibility; secure capture and lock authorization are owned by spec 004.

## Requirements

- **R1. Hardware session:** support DRM/KMS with logind/libseat lifecycle, VT switching, seat/device ownership, suspend/resume, GPU reset diagnostics, and safe startup/shutdown.
- **R2. Output management:** enumerate and hotplug outputs; preserve visible window placement through connect/disconnect, resolution/scale/primary changes, and resume; support at least the declared reference multi-output configurations.
- **R3. App compatibility:** run representative GTK, Qt, and XWayland clients; document exposed Wayland globals and XWayland integration/version limits.
- **R4. Input and data:** support keyboard layouts, pointer, touch and declared tablet support, relative pointer/constraints, text input/IME, clipboard, and drag/drop.
- **R5. Scaling:** support per-surface preferred scale and fractional scaling paths where available. Native Wayland and XWayland geometry, rendering, and input coordinates remain aligned at tested scale factors.
- **R6. Rendering fallback:** use DMA-BUF, explicit sync, presentation time, hardware cursor, and direct scan-out only when supported and correct; fall back safely and report capability/diagnostics.
- **R7. Mixed refresh:** test mixed 60/120 Hz outputs, fullscreen policy, and VRR-capable hardware. Tearing hints, VRR support, and observed presentation behavior are reported separately.
- **R8. Support matrix:** publish supported distributions, kernels, GPUs/drivers, outputs, protocol versions, known limitations, and tested fallbacks.

## Constraints

- Pin Smithay and protocol revisions per release and maintain a machine-readable global exposure/support matrix.
- No universal zero-latency, sharpness, VRR, HDR, or direct-scanout guarantee.
- Screen capture and screenshots must use the portal and compositor authorization defined by spec 004; do not expose an untrusted direct capture path.
- Color/HDR management and advanced per-output VRR policy remain later work unless selected as an explicit supported-matrix requirement.

## Acceptance criteria

- **A1. Hardware lifecycle:** repeated login, VT switch, suspend/resume, output hotplug, and supported GPU reset fault journeys do not leave devices or outputs unusable without a diagnosed fatal session failure.
- **A2. App matrix:** GTK, Qt, and XWayland smoke journeys cover map, resize, focus, fullscreen, clipboard, drag/drop, and IME where applicable.
- **A3. Scaling alignment:** native Wayland and XWayland test clients stay correctly positioned and input-aligned through 100%, 125%, 150%, and 200% where supported, including output hotplug.
- **A4. Multi-output:** declared mixed-resolution/scale and mixed 60/120 Hz cases pass placement and presentation checks; fallback behavior is documented.
- **A5. Diagnostics:** unsupported protocol/hardware paths report an actionable reason and choose a tested fallback.
- **A6. Support matrix:** every claimed configuration has a test result, software versions, and limitations; untested configurations are not marketed as supported.

## Dependencies and risks

Requires the nested lifecycle and state model from spec 001. Hardware farm availability, drivers, logind/libseat integration, XWayland behavior, and mixed-DPI fidelity are major schedule risks. Locking and portals must be completed by spec 004 before daily-driver release regardless of hardware-spec completion.
