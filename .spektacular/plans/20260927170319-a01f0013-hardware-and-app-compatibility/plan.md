---
created_date: "2026-09-27"
document_status: draft
---

# Plan — hardware session and app compatibility

**Status:** Draft plan  
**Spec:** `hardware-and-app-compatibility`

## Phase 1 — support matrix and backend design

- Turn spec 000 hardware/protocol findings into a named target matrix and test farm plan.
- Decide session/device lifecycle with logind/libseat, DRM/KMS backend, VT switching, and error recovery.
- Define output/workspace/window-placement model and capability diagnostics.

**Gate:** supported and explicitly unsupported hardware paths are reviewable before broad backend work.

## Phase 2 — single-output hardware session

- Implement hardware session ownership, output enumeration, modesetting, seat/input lifecycle, and clean teardown.
- Add GPU reset, VT switch, suspend/resume, and output loss diagnostics/fallback.
- Verify representative GTK/Qt clients on native Wayland.

## Phase 3 — multi-output and app interoperability

- Add hotplug, primary/output changes, mixed resolution/scale, workspace and window recovery.
- Integrate XWayland and test common clients, fullscreen, focus, clipboard, DnD, and geometry.
- Implement IME, keyboard layouts, pointer constraints/relative motion, touch/tablet support as declared.

## Phase 4 — scale, frame, and fallback matrix

- Validate native and XWayland scaling at 100/125/150/200% where supported.
- Exercise DMA-BUF, explicit sync, presentation feedback, hardware cursor/direct scan-out, and software/GL fallback.
- Test mixed 60/120 Hz and VRR-capable path; measure presented behavior rather than infer support from protocol advertisement.

## Exit gate

All acceptance journeys pass for every claimed matrix configuration; publish exact protocol globals/versions, drivers, fallbacks, and known limitations.
