---
created_date: "2026-09-27"
document_status: draft
---

# Research — performance, packaging, and release readiness

**Status:** Draft; release implementation research pending.

## Questions

- Which package/session mechanisms support safe rollback and last-known-good config on each selected distro?
- How will trace collection report p95/p99 with reproducible workload, warm/cold state, and noise handling?
- What hardware farm covers integrated/discrete GPU, mixed scale/refresh, suspend/resume, and XWayland?
- Which protocol conformance tools and wlcs cases match pinned protocol revisions, and what project probes are missing?
- What privacy-safe logs/artifact retention and dependency provenance are required by supported distros?

## Evidence

Benchmark recipe and environment must derive from spec 000; package behavior must be verified in clean VMs/real target distributions. Report exact source revision and avoid using synthetic nested performance as a proxy for full hardware session performance.
