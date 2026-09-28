---
created_date: "2026-09-27"
document_status: draft
---

# Plan — program baseline and technical spikes

**Status:** Draft plan; evidence not yet collected  
**Spec:** `program-baseline-and-spikes`

## Phase 1 — choose baseline and workload

- Resolve reference distro/image, GNOME release, kernel, GPU/driver, monitor topology, power mode, locale, services, and extension/config baseline.
- Define equivalent user journeys and cold/warm startup sequence; document sample count, reboot/cache policy, and noisy-run rule.
- Create a trace/artifact format and privacy redaction policy.

**Gate:** a second operator can reproduce the exact baseline environment and workload.

## Phase 2 — measure GNOME baseline

- Automate startup, idle CPU/PSS, interaction workload, frame pacing, missed deadlines, and soak collection.
- Add external input-to-photon measurement if available; otherwise label timestamp metrics as a proxy.
- Run repetitions; retain raw traces and report variance and environmental metadata.

**Gate:** no target regression threshold is approved until baseline distributions and noise are understood.

## Phase 3 — toolkit and compositor spikes

- Implement equivalent minimal GTK4/libadwaita and Rust-native overview/panel prototypes.
- Test AT-SPI via screen reader, keyboard navigation, large text, reduced motion, high contrast, IME, RTL, and layer surface behavior.
- Run a Smithay nested compositor with representative GTK and Qt clients; inventory protocol/version gaps and event-loop ownership.

**Gate:** toolkit decision records UX/a11y gaps and measured costs; prototype backend decision records supported assumptions.

## Phase 4 — threat model and decisions

- Map process trust boundaries, same-UID threat model, sensitive assets, lock/capture authorities, extension capabilities, crash boundaries, and failure modes.
- Review against security-sensitive release journeys and identify specialist reviewers.
- Write or update ADRs for baseline, toolkit, nested backend/protocol pin, and unresolved production identity design.

**Exit evidence:** spec 000 A1–A6 artifacts exist; implementation speks have concrete inputs or explicit blocking decisions.

## Deferred

Do not ship compositor code, claim performance wins, select production GPU support, or close secure identity decisions from a nested prototype alone.
