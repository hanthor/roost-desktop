---
created_date: "2026-09-27"
document_status: draft
---

# Plan — secure session, accessibility, and system integration

**Status:** Draft plan  
**Spec:** `secure-session-accessibility-integration`

## Phase 1 — security and ownership design

- Approve threat model, same-UID process identity, service launch/credential flow, privileged surface allowlist, and audit boundary.
- Draw lock/auth and capture/portal grant/revocation sequences; identify compositor-owned state and helper responsibilities.
- Complete service ownership/timeouts map and settings compatibility map proposal.

**Gate:** independent security review accepts the trust model and fail-closed state machine before production lock/capture code.

## Phase 2 — accessibility and service prototypes

- Verify AT-SPI and screen-reader paths for shell, lock, and recovery toolkit choices.
- Define notification lock redaction, screen reader announcements, large-text/high-contrast/reduced-motion, RTL, touch, IME, and on-screen keyboard journeys.
- Implement adapters to existing services with typed status, bounded calls, and restart behavior.

## Phase 3 — lock/auth and portal implementation

- Implement compositor lock latch and ext-session-lock-v1 behavior; preserve lock across UI loss.
- Implement PAM-facing helper with narrow unlock assertion; validate assertion inside compositor.
- Implement portal consent/source-selection and capture grant, PipeWire delivery, revocation, and locked-session rules.

## Phase 4 — fault/security/accessibility qualification

- Exercise UI/helper/backend crashes, stalls, restart, VT switch, suspend/resume, hotplug, spoofing, stale assertions, unauthorized capture, and secret scanning.
- Run end-to-end assistive technology and localization journeys on supported configurations.
- Publish settings compatibility map and release-blocking discrepancy list.

**Exit gate:** every acceptance criterion passes and specialist security/a11y review has no unresolved critical finding.
