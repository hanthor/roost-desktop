---
created_date: "2026-09-27"
document_status: draft
---

# Test plan — performance, packaging, and release readiness

**Status:** Draft  
**Strategy:** [Cross-cutting test strategy](../../../docs/test-strategy.md)

## Cases and evidence

- Re-run spec 000 benchmark suite on same machine/config/workload for pinned GNOME and RWD. Record p50/p95/p99, sample count, variance, startup, idle CPU, whole-session PSS, frame times/misses, input latency/equipment, GPU memory, and 24-hour soak growth.
- Run per-change build/format/lint/unit/property/protocol/nested integration in CI; nightly fuzz, extended fault/restart and soak; release-candidate hardware and accessibility/security runs.
- Per supported distro, test clean install, session select, login/logout, upgrade, broken shell package/config rollback, last-known-good restore, uninstall, and preservation of user/app data.
- Verify test artifact provenance, raw trace retention/redaction, dependency/protocol manifest, and support-matrix completeness.
- Fail release on unresolved critical lock/capture/input/clipboard/IME/accessibility issue, missing support evidence, unexplained unbounded growth, or recovery failure. Rerun noisy measurements using the declared rule; retain original data.
