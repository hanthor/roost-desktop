---
created_date: "2026-09-27"
document_status: draft
---

# Shell workflow parity

**Status:** Draft  
**Parent:** [Program architecture](../../docs/architecture.md)  
**Depends on:** 000 program baseline and technical spikes; 001 nested compositor and shell recovery.

## Overview

Implement the familiar everyday shell workflows that make the session usable: Activities/overview, app grid and favorites, search, panel and quick settings, workspace/window switching, and notifications. Match the pinned GNOME baseline by explicit journeys and a discrepancy ledger. Optional tiling, alternative docks, themes, and new workspace models are excluded until default journeys pass.

## Requirements

- **R1. Activities and overview:** Super, Activities control, and configured hot corner open the overview; it presents windows, workspaces, favorites/running apps, and search. Repeating the trigger or Escape returns predictably to the prior app.
- **R2. Launch/search:** launch desktop entries from grid and results; pin/unpin favorites; switch to an existing instance; show launch feedback. Search providers run asynchronously and cannot block typing, focus, or frame scheduling.
- **R3. Panel and quick settings:** clock/calendar and supported network, audio, Bluetooth, media, power, brightness, and volume controls show loading, disconnected, and error states using existing service APIs.
- **R4. Window/workspace workflows:** dynamic workspaces, move-window actions, Alt-Tab, fullscreen/maximize/snap, keyboard shortcuts, touchpad gestures, transient/dialog parent behavior, and multi-monitor placement follow the documented parity ledger.
- **R5. Notifications:** freedesktop-compatible notifications support banners, history, Do Not Disturb, actions, expansion, dismissal, calendar integration, and no focus theft; private content policy is delegated to secure-session rules when locked.
- **R6. Focus authority:** shell sends user intent; compositor validates activation tokens and owns focus/geometry. Animation and pointer interactions do not require per-motion IPC round-trips.
- **R7. UX/localization:** journeys cover keyboard-only use, large text, reduced motion, high contrast, touch target sizing, RTL, and localization; deviations are explicit.
- **R8. Service boundaries:** visible control ownership is mapped to existing system services and the notification service. Shell UI does not replace networking, audio, power, or notification infrastructure.

## Constraints

- Use the pinned GNOME baseline selected in spec 000 and report current upstream behavior, not old issue reports.
- Toolkit choice follows the spec 000 spike and must satisfy AT-SPI and IME journeys.
- Search, service discovery, icon loading, and extension-provided results use bounded asynchronous work.
- Tiling, third-party theme injection, and GNOME Shell extension compatibility are out of scope.

## Acceptance criteria

- **A1. Journey matrix:** scripted and human-reviewed overview/search/dash, app launch, panel/quick settings, workspaces/window switching/gestures, and notification journeys pass against the pinned baseline or have accepted discrepancy entries with severity and owner.
- **A2. Search responsiveness:** slow/failing search providers do not block typing, application focus, or compositor input; provider results are cancellable and bounded.
- **A3. Existing services:** controls operate against supported existing services and provide understandable unavailable/error states without hanging the shell.
- **A4. Input and focus:** every action that changes focus follows compositor token policy and is tested for stale/replayed authorization.
- **A5. Accessibility/localization:** keyboard, screen-reader, large text, reduced motion, high contrast, touch, and RTL journeys are recorded and reviewed.
- **A6. Evidence:** screenshots or recordings and the discrepancy ledger are versioned with baseline version and test environment.

## Dependencies and risks

Requires compositor shell-control API and a stable snapshot/change model from spec 001. Notification storage/service and several controls depend on system integration in spec 004. A mismatch between control API granularity and animation needs is an architecture risk; resolve with measured prototypes instead of high-frequency control messages.
