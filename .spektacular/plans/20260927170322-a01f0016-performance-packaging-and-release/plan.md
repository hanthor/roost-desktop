---
created_date: "2026-09-27"
document_status: draft
---

# Plan — performance, packaging, and release readiness

**Status:** Draft plan  
**Spec:** `performance-packaging-and-release`

## Phase 1 — reproducible CI and artifact model

- Convert spec 000 benchmark recipe into versioned scripts/configuration and stable artifact manifests.
- Add per-change build/lint/unit/property/protocol/nested lifecycle checks; define privacy-safe failure retention.
- Add nightly protocol/fuzz/soak and schedule hardware farm runs.

## Phase 2 — distro packaging and rollback

- Select supported distro/session integration from spec 003 matrix.
- Build clean install, upgrade, session selection, uninstall, atomic config/last-known-good, and recovery package flows.
- Inject broken shell package/config and prove path to login-capable recovery without user-data deletion.

## Phase 3 — release candidate verification

- Run full app/protocol/accessibility/security/hardware matrix and 24-hour soak.
- Compare whole-session GNOME/new-session traces under identical workload and report p50/p95/p99, variance, misses, PSS, CPU, GPU memory, and startup.
- Resolve blockers; approve evidence-backed threshold exceptions only by ADR.

## Phase 4 — release dossier

- Publish support matrix, tested protocol/Smithay versions, dependency manifest, benchmark recipes/raw traces, test report, known limitations, upgrade/recovery instructions, and session-failure boundary.

**Exit gate:** all 1.0 gates from specs 002–004 pass on supported configurations; no undocumented support or performance claims remain.
