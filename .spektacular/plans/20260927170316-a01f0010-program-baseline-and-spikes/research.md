---
created_date: "2026-09-27"
document_status: draft
---

# Research — program baseline and technical spikes

**Status:** Draft; research still required.

## Confirmed starting points

- Smithay documents a calloop-centered compositor event model and provides backend/protocol helpers; project-specific window management, drawing, policy, and hardware validation remain project work. Source: https://smithay.github.io/smithay/smithay/index.html
- GTK exposes accessibility through AT-SPI support in the toolkit; a non-GTK candidate must demonstrate equivalent end-to-end behavior. Source: https://developer.gnome.org/documentation/guidelines/accessibility.html
- GNOME Shell extensions execute as part of the shell process; this project intentionally chooses an out-of-process capability boundary. Source: https://extensions.gnome.org/about/
- Spektacular's documented workflow is spec → plan → implement, with plan, context, and research artifacts. Source: https://github.com/hivecommons/spektacular

## Research still required

1. Select and pin baseline GNOME release/image and hardware; record exact settings and equivalent journeys.
2. Identify current upstream protocol versions and Smithay release support relevant to nested, hardware, lock, capture, fractional scale, explicit sync, and XWayland work.
3. Compare selected GTK4/libadwaita and Rust-native candidate under the same shell tasks; include screen reader/AT-SPI, IME, RTL, layer surfaces, and resource measurements.
4. Retrieve and verify GNOME GitLab issues through current direct/API access; record state, MR, release, reproduction, and test mapping per `docs/research/README.md`.
5. Review threat model with Wayland/compositor security and distribution identity experts.

## Evidence rule

Date/version every implementation-sensitive source. Mark inaccessible/upstream issue state as unverified and keep it out of claims.
