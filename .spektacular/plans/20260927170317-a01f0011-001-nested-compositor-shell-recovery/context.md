---
created_date: "2026-09-27"
document_status: draft
---

# Context — nested compositor and shell recovery

**Status:** Draft; no implementation exists yet.

## Repo and intended component boundaries

Registered code root: `/home/ubuntu/dev/rust-wayland-desktop`. Planned workspace components: `compositor/`, `shell-control-schema/`, `shell-host/`, `integration-tests/`, and `docs/`. These are planning targets, not existing directories.

Compositor owns window/seat/workspace/input state. Shell host is a Wayland client plus a separate local control connection. The process supervisor and minimal recovery affordance must not depend on the failed shell. Applications connect to compositor directly and must remain connected through shell restart.

## Existing design inputs

Parent architecture: `docs/architecture.md`; sequencing/traceability: `docs/roadmap.md`; spek: `001-nested-compositor-shell-recovery`.

## Provisional constraints

Nested developer preview only. No DRM/KMS takeover, XWayland, portals, secure lock, production same-UID security claim, tiling, full desktop parity, or public extension API. Smithay/calloop is the initial event model. Preserve a path to later toolkit selection; shell host toolkit is not fixed by this slice.

## Dependencies and downstream impact

Spec 000 supplies nested backend and protocol findings. Specs 002/003 depend on the shell control/state contract. Spec 004 depends on the trust model but may not treat prototype credentials as production proof.
