---
created_date: "2026-09-27"
document_status: draft
---

# Context — program baseline and technical spikes

**Status:** Draft. This context is for planning; it does not describe shipped behavior.

## Project shape

This is a new independent desktop project. There is no compositor codebase yet; the registered repo root is `/home/ubuntu/dev/rust-wayland-desktop`. Specs and plans live in Spektacular. Source implementation directories will be created when spec 001 is approved for implementation.

## Binding direction

- Rust compositor built with Smithay; begin with Smithay/calloop event ownership.
- Ordinary shell is a supervised Wayland client; compositor remains authoritative for focus/window/output/input policy.
- GTK4/libadwaita and a Rust-native UI candidate must be compared on accessibility and UX evidence.
- Same-hardware current GNOME behavior is the baseline. Old issue reports are not treated as current defects.
- A11y, lock, portal and hardware compatibility are release gates, not optional polish.

## Provisional assumptions

Reference distro, GNOME version, GPU, display, exact supported services, toolkit, and same-UID attacker scope remain undecided. Nested work may use a provisional development setup, but no product support or security claim may depend on those assumptions.

## Downstream consumers

Specs 001–006 need baseline metadata, protocol-gap inventory, threat model, and explicit resolved/unresolved decisions. Specs 002 and 004 particularly rely on the accessibility spike.
