---
created_date: "2026-09-27"
document_status: draft
---

# Context — hardware session and app compatibility

**Status:** Draft; no hardware backend implementation exists.

## Ownership

This spek owns Smithay backend integration, DRM/KMS/libseat/logind lifecycle, outputs, app-facing protocol compatibility, XWayland, scaling, input devices, data devices, and rendering fallbacks. Spec 004 owns capture consent, lock security, and privileged surface authorization. Spec 006 owns whole-release benchmark and packaging gates.

## Dependencies

Spec 000 target hardware and protocol gap inventory; spec 001 compositor state/lifecycle and control schema. Shell parity work from spec 002 provides end-to-end user journeys but hardware work can proceed in parallel after the 001 contract stabilizes.

## Unknowns

Reference GPU/driver/distro, available hardware farm, supported tablet/device set, target XWayland version, exact protocol global versions, and color/VRR release scope. Do not convert these into support claims before evidence exists.

## Constraints

Capability-based fallbacks; protocol support is not hardware correctness. Preserve visible windows through hotplug and resume. Capture remains portal-controlled.
