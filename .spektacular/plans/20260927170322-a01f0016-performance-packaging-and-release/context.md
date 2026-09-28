---
created_date: "2026-09-27"
document_status: draft
---

# Context — performance, packaging, and release readiness

**Status:** Draft; no release pipeline or package exists.

## Dependencies and boundary

Spec 000 creates the benchmark reference. Specs 001–004 produce nested, shell, hardware, secure-session, and accessibility behavior. Spec 005 is optional unless extensions are explicitly in release scope. This spec integrates and verifies results; it does not waive an owning spec's acceptance criteria.

## Project targets

On the named reference machine, target no more than 10% regression in p95 interactive latency and total session PSS versus pinned GNOME, no persistent frame pacing regression at 60/120 Hz, and no unexplained unbounded growth during 24-hour soak. Targets are provisional until baseline measurements, and any changes need a recorded evidence-backed ADR.

## Unknowns

Supported distros, package formats, session entry conventions, rollback mechanism, hardware farm access, latency measurement equipment, and CI resources remain to be selected. Compositor crash remains a session failure and must be explicit in release notes.
