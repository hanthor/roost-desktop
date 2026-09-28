---
created_date: "2026-09-27"
document_status: draft
---

# Plan — shell workflow parity

**Status:** Draft plan  
**Spec:** `shell-workflow-parity`

## Phase 1 — parity contract and design

- Pin baseline journeys from spec 000 and maintain a behavior/discrepancy ledger.
- Confirm shell toolkit using keyboard, AT-SPI, layer-surface, IME, RTL, large-text, and reduced-motion evidence.
- Define overview state/animation intents, navigation/focus-return rules, app identity/favorites, and notification ownership.

**Gate:** user-visible behavior and accessibility journeys are reviewable before broad UI work.

## Phase 2 — overview, launcher, and search

- Implement Activities triggers, windows/workspaces overview, app grid/favorites, launch feedback, and instance switching.
- Build async/cancellable search fanout with per-provider timeout/quota and deterministic ordering.
- Verify compositor remains responsive while providers stall or fail.

## Phase 3 — panel, controls, and notifications

- Implement clock/calendar, indicators, quick settings, media, DND, notification banners/history/actions.
- Integrate existing system-service APIs through explicit adapters; model disconnected/error/loading states.
- Confirm focus behavior, keyboard operation, screen reader announcements, touch, RTL, large text, and reduced motion.

## Phase 4 — parity journeys and evidence

- Run scripted and human-reviewed pinned-baseline journeys across supported configurations.
- Capture screenshots/recordings and discrepancy ledger; fix release-blocking deviations.
- Review latency/frame traces and prove no provider/service request blocks input or compositor scheduling.

**Exit gate:** spec 002 acceptance passes or every deviation has documented owner, severity, and release disposition.
