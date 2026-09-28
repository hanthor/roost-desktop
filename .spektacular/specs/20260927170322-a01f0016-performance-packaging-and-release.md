---
created_date: "2026-09-27"
document_status: draft
---

# Performance, packaging, and release readiness

**Status:** Draft  
**Parent:** [Program architecture](../../docs/architecture.md)  
**Depends on:** 000 baseline; 001–004 for a daily-driver candidate. Spec 005 is optional unless public extension distribution is part of the release.

## Overview

Turn the program's prototypes into a supportable release candidate with comparable performance evidence, conformance/fault coverage, reproducible packaging, bounded soak results, and a working recovery path from failed upgrade or configuration. Performance budgets are derived from measured baseline data and may change only through a recorded, evidence-backed decision.

## Requirements

- **R1. Comparable release benchmarks:** repeat spec 000 measurements on the complete desktop and pinned GNOME baseline with identical hardware, kernel, GPU/driver, displays, apps, settings, and workload.
- **R2. Performance targets:** target no more than 10% regression in reference-machine p95 interactive latency and whole-session PSS; no persistent frame-pacing regression at 60/120 Hz; no unexplained growth over 24-hour soak. Report variance, raw traces, and any justified threshold changes.
- **R3. CI/conformance:** formatting, lint, build, unit/property tests, protocol probes, nested integration, and client smoke tests run in CI; hardware farm and long soak run on release candidates/scheduled builds.
- **R4. Fault/security evidence:** retain privacy-safe artifacts for compositor, shell, extension, portal, lock, worker, GPU reset, unplug, suspend/resume, low memory/disk, and failed update journeys.
- **R5. Packaging/session integration:** produce supported distro packages, session entry, dependencies, upgrade path, logs/diagnostics, and uninstall behavior without replacing the display manager or core services.
- **R6. Rollback:** broken shell package/configuration recovers to a login-capable safe session without deleting app or user configuration data; config writes are atomic and last-known-good data is preserved.
- **R7. Release provenance:** publish source revision, dependency/protocol versions, SBOM or equivalent dependency manifest, test matrix/results, known issues, support matrix, and release notes.
- **R8. Claims:** performance, security, accessibility, and hardware support claims link to evidence and state their machine/configuration scope. Compositor crash is documented as session failure.

## Constraints

- The 10% thresholds are project targets and can change only through a recorded ADR backed by repeated comparable measurements.
- No release gate passes on a single favorable sample; noisy runs are flagged and rerun under documented rules.
- Full 1.0 requires specs 002, 003, and 004 acceptance; 005 remains optional unless explicitly added to the release scope.
- A compositor crash is not described as seamless app survival.

## Acceptance criteria

- **A1. Benchmark reproducibility:** independent rerun produces the same metric set, with environment and raw traces identified.
- **A2. Performance gate:** target thresholds pass on the named reference configuration or have an approved evidence-backed exception; soak shows no monotonic unbounded growth outside documented bounded caches.
- **A3. Verification matrix:** CI, protocol, application, accessibility, security, hardware, and fault evidence is complete for every supported configuration and traceable to specs.
- **A4. Upgrade recovery:** intentionally broken shell package/config returns to a working recovery/login path without deleting user data.
- **A5. Packaging:** clean install, upgrade, rollback, session selection, and removal are documented and tested on each supported distro.
- **A6. Release dossier:** support matrix, dependency/protocol versions, test/benchmark artifacts, limitations, and session-failure behavior are published.

## Dependencies and risks

Requires stable daily-driver behavior from specs 002–004 and reference benchmark infrastructure from 000. Hardware availability and distribution packaging can dominate release timing. Do not use benchmark targets to justify deferring lock, accessibility, or compatibility gates.
