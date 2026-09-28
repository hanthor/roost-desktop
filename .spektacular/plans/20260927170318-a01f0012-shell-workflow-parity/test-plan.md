---
created_date: "2026-09-27"
document_status: draft
---

# Test plan — shell workflow parity

**Status:** Draft  
**Strategy:** [Cross-cutting test strategy](../../../docs/test-strategy.md)

## Cases and evidence

- Record pinned-GNOME baseline and RWD journey pairs for overview trigger/return, windows/workspaces, grid/favorites/launch, search, controls, and notifications.
- For search, inject slow, failing, cancelled, oversized, and high-rate providers; assert typing, focus, and compositor input stay responsive and results are bounded.
- Exercise service absent/restart/error states for each quick-setting control; no modal focus theft or indefinite spinner.
- Verify notification banner/history/actions/DND/calendar, keyboard dismissal/expansion, no focus theft, and lock redaction via spec 004.
- Human-review AT-SPI screen-reader announcements and keyboard journey; cover large text, contrast, reduced motion, touch, RTL, and locale switching.
- Keep screenshots/recordings, trace tags, and discrepancy ledger tied to GNOME baseline version and RWD commit.
