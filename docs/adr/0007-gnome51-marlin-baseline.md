# ADR 0007: GNOME 51 baseline and TunaOS Marlin target

- Status: Decided
- Decision date: 2026-10-01
- Recorded: 2026-10-04
- Owner: project
- Related issues: #64 baseline capture, #69 packaging, #68 VM verification, #73 performance

## Context

The user selected GNOME 51 as the parity baseline and TunaOS Marlin as the first deployment target. Comparing desktops on different distributions would mix shell differences with kernel, Mesa, font and service differences. The roadmap and testing strategy already use this choice; the decision register must point to it too.

## Decision

Use GNOME 51 from `ghcr.io/tuna-os/marlin:gnome` as the reference. Capture each comparison bundle with the resolved image digest and the VM profile. A mutable image tag alone does not identify a repeatable baseline.

Build Tuna Desktop as a Marlin desktop variant on the same Arch-based bootc image family, initially on x86_64. Build Arch packages in `tuna-os/tunaos-packages`; keep the Debian package for developer hosts and package-layout verification. Debian/Ubuntu packages do not establish the Marlin release gate.

Use GNOME for shell appearance and behavior. Niri is the reference for Tuna Desktop's optional scrollable tiling mode, and COSMIC can inform alternatives without redefining GNOME parity. Record accepted deviations and unverified claims in the parity ledger.

## Alternatives

A Debian/Ubuntu reference would simplify local package installation but would not test the selected Marlin target. Comparing against an unspecified or remembered GNOME release would give no reproducible baseline. Both alternatives were rejected by the selected target and baseline.

## Consequences and remaining gates

Reference and candidate comparisons use the same VM hardware profile and record image digests. The Marlin VM lane is evidence for that virtual configuration only. The physical GPU, kernel, output and input support matrix remains open; this decision does not assert broad hardware support or daily-driver readiness.

See [the program roadmap](../roadmap.md), [the testing strategy](../test-strategy.md#3a-parity-evidence-added-2026-10-01), [the parity ledger](../parity-ledger.md), and the Marlin packaging definition in `packaging/marlin/Containerfile`.
