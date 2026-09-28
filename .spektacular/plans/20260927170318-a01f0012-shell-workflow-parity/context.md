---
created_date: "2026-09-27"
document_status: draft
---

# Context — shell workflow parity

**Status:** Draft; shell implementation not started.

## Boundaries

Own user-visible shell workflows and parity behavior. Compositor focus/window decisions remain with spec 001. Existing services and notification transport are integrated by spec 004; this spec owns shell control presentation and interaction. Hardware-specific placement belongs to spec 003.

## Required inputs

- Pinned GNOME version/config and traceable journey ledger from spec 000.
- Stable shell-control snapshot/change and command contract from spec 001.
- Toolkit/accessibility spike decision from spec 000.
- Service adapter and notification ownership map from spec 004 planning.

## Product constraints

Default workflows are recognizable and parity-first. Tiling, alternative docks, theme injection, and legacy GNOME Shell extension compatibility are excluded. User-visible deviations must be recorded instead of silently accepted.

## Risks

Full shell UI scope is large; search/service integration can block perceived responsiveness; parity may vary by GNOME version. Keep visible behavior and backend service integration separately testable.
