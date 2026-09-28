---
created_date: "2026-09-27"
document_status: draft
---

# Research — hardware session and app compatibility

**Status:** Draft; current version-specific research is pending.

## Research questions

- Which Smithay backend APIs and DRM/KMS/session support exist in the pinned release?
- Which distributions expose required logind/libseat behavior and session entry conventions?
- Which Wayland protocol revisions are implemented by target GTK/Qt/XWayland clients?
- What are current native XWayland scaling capabilities and fallbacks on the pinned GNOME baseline?
- What device/driver combinations support explicit sync, DMA-BUF modifiers, presentation feedback, hardware cursor, direct scan-out, VRR, and mixed refresh?
- Which wlcs/protocol tests apply, and where are gaps requiring project probes?

## Evidence requirements

Use versioned upstream Smithay/protocol documentation and test on each claimed hardware combination. Record exact topology, modes, scale, kernel, firmware, GPU, driver, and client version. Distinguish protocol advertisement, client buffer behavior, and observed presentation results.

- Smithay: https://smithay.github.io/smithay/smithay/index.html
- Wayland protocol definitions: https://gitlab.freedesktop.org/wayland/wayland-protocols
- xdg-desktop-portal documentation for capture authority is planned in spec 004 research.
