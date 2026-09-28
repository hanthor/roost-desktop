---
created_date: "2026-09-27"
document_status: draft
---

# Context — secure session, accessibility, and system integration

**Status:** Draft; production lock/portal implementation has not started.

## Authority model

Compositor owns the lock latch, input routing, privileged globals, and final validation of capture authorization. Portal backend handles user-facing consent/source selection and mediates PipeWire delivery. A reviewed PAM-facing helper performs authentication and returns a narrow assertion. The exact identity/capability mechanism must come from spec 000 threat-model review, not UID/app-ID assumptions.

## Dependencies

- Spec 000: threat model, same-UID assumptions, toolkit/accessibility findings.
- Spec 001: compositor state/control and process recovery.
- Spec 002: user-facing shell workflows and notification surfaces.
- Spec 003: real seats, VT, output, suspend/resume, input, and GPU lifecycle.

## Release constraints

Secure lock/auth, portal consent/revocation, accessibility, and service identity are 1.0 gates. Services are reused; this project does not replace display manager, network, audio, or power daemons. Any unresolved critical security or accessibility gap blocks release.
