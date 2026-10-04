# Roost roadmap

The [GitHub Project](https://github.com/users/hanthor/projects/4) is the live work queue. The [program roadmap](docs/roadmap.md) owns delivery gates and requirement traceability; the [parity ledger](docs/parity-ledger.md) owns behavior, evidence and deviations. GNOME 51 on TunaOS Marlin is the reference, with niri used for the optional scrollable tiling mode ([ADR 0007](docs/adr/0007-gnome51-marlin-baseline.md)).

Complete the remaining work in this order. These are delivery stages, not calendar commitments.

1. **Safe real session.** Complete Marlin login and portal integration (#68, #69, #223–#225), shortcut inhibition consent (#221), lock/authentication and suspend/VT/logout verification (#62), and capture consent and revocation (#61).
2. **GNOME behavior and visuals.** Close the overview, app-grid, input, audio, network, battery, animation and keybinding gaps (#206–#220). Finish the settings compatibility map (#63), capture missing GNOME baseline evidence (#208), and measure niri-mode visuals (#194).
3. **Reliability and measured release readiness.** Run stress/fuzz/endurance tests (#203), compare performance with GNOME on the same Marlin VM (#73), complete the physical hardware matrix, and verify image upgrades and rollback. Keep limitations explicit until their evidence passes.
4. **Later capabilities.** Broaden GPU and device coverage, colour/HDR/VRR work and optional public extension distribution through the architecture's later tier. They do not substitute for the earlier security and usability gates.

An item is complete only when its acceptance criteria have implementation and passing evidence. Keep parent issues open while their hardware or integration acceptance remains unverified. The project is a developer preview while these release gates remain open.
