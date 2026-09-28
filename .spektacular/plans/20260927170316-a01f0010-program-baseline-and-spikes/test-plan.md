---
created_date: "2026-09-27"
document_status: draft
---

# Test plan — program baseline and technical spikes

**Status:** Draft  
**Strategy:** [Cross-cutting test strategy](../../../docs/test-strategy.md)

## Cases and evidence

- **RWD-000-R1/R2:** reproduce pinned GNOME environment/workload on a second run; compare manifest equality and retain raw startup, CPU/PSS, frame, latency/proxy, GPU memory, and soak artifacts with variance.
- **RWD-000-R3:** equivalent GTK and Rust-native prototype journeys; keyboard-only and AT-SPI screen-reader completion for each, plus IME, RTL, large text, reduced motion, layer surfaces, and resource/frame observations.
- **RWD-000-R4:** nested map/focus/input/frame smoke with GTK/Qt clients; log exact protocol globals and versions and a testable gap list.
- **RWD-000-R5:** tabletop threat-model review with adversarial cases for same-UID shell spoof, privileged layer spoof, lock UI death, unauthorized capture, secret exposure, and extension compromise.
- **RWD-000-R6:** ADR audit verifies evidence links, unresolved decision owner, and dependent speks.

Do not set performance pass/fail thresholds from this spike; the output is the measured baseline distribution and test recipe.
