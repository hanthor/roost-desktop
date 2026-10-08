# Program architecture

**Status:** Proposed, for design review  
**Date:** 27 September 2026  
**Name:** Tuna Desktop (Rust Wayland desktop session), formerly Roost. Crates, binaries, packages and environment variables still use the `tuna` name until the internal rename ([#505](https://github.com/tuna-os/tuna-desktop/issues/505)).

## Product decision

Build a new desktop session with a familiar GNOME-inspired overview, panel, quick settings, applications, notifications, workspaces, lock screen, and accessibility behavior. It is an independent desktop, not a drop-in GNOME Shell rewrite. It launches existing GTK/libadwaita applications and does not load existing GNOME Shell extensions, implement private Mutter APIs, or claim GNOME upstream status without agreement.

The parity target is a named, pinned GNOME release and configuration on the same hardware. Tiling and third-party extensions are optional capabilities; neither is a prerequisite for familiar default workflows.

## Process and authority boundaries

- **Compositor:** Rust process built with Smithay. Owns seats, outputs, focus, stacking, workspace/window state, input routing, frame scheduling, capture authorization, and privileged protocol exposure. It remains authoritative when the shell is slow or disconnected.
- **Shell host:** one supervised Wayland client initially, containing panel, overview, launcher, and quick settings. It draws its own surfaces and sends intent through a versioned control API. It never owns focus or window policy.
- **Recovery and lock:** minimal recovery affordance is available when the shell is absent. Lock state remains in the compositor; unlock requires a reviewed authentication path. Production credential binding and lock implementation are release-gate decisions.
- **Extensions:** separate, untrusted processes behind a capability broker. No privileged compositor Wayland socket, arbitrary native plugin ABI, raw input, clipboard, capture, filesystem, or network access by default. Wasmtime is a later optional worker implementation, not the security boundary by itself.
- **System integration:** reuse system services and portals where appropriate. The portal backend mediates consent; compositor-enforced grants authorize capture and are revocable.

Use Smithay/calloop's event model initially. Pointer/input/frame paths must not synchronously wait for shell, extensions, search, disk, or network work. The shell toolkit choice is GTK4/libadwaita ([ADR 0006](adr/0006-gtk4-libadwaita-shell.md)); accessibility and visual parity remain acceptance criteria.

## Delivery tiers

1. **Nested developer preview:** nested compositor, floating windows, focus/workspaces, one shell host, overview trigger/window list, restart and resynchronization, GLES-over-EGL rendering, lock prototype, lifecycle harness. The pinned nested backend has no software-rendering fallback, so this tier requires a working EGL implementation ([ADR 0001](adr/0001-nested-backend-smithay-pin.md)); Mesa llvmpipe satisfies it in VMs and CI. Not a secure daily-driver session.
2. **Daily-driver candidate:** DRM/KMS via logind/libseat, multi-output/hotplug, XWayland, clipboard/drag-and-drop, IME/accessibility, secure lock/auth, suspend/resume, portals, settings/session integration, support matrix, conformance and fault tests. Ship only after security/usability gates.
3. **Later measured work:** tiling variants, VRR/HDR/color improvements, public extension distribution, remote desktop, more GPU configurations.

## Program speks

The delivery order, dependencies, acceptance-evidence map, decision gates, and research rules are maintained in [the program roadmap](roadmap.md). The Spektacular registry contains:

0. Program baseline and technical spikes.
1. Nested compositor and shell recovery (first buildable vertical slice).
2. Shell workflow parity.
3. Hardware session and application compatibility.
4. Secure session, accessibility, and system integration.
5. Extension broker and public API (optional post-core capability).
6. Performance, packaging, and release readiness.

Each spek has its own draft `plan.md`, `context.md`, and `research.md`; plans remain drafts until dependencies and evidence are reviewed. The first spek does not claim full desktop parity.

## Cross-cutting release gates

- Apps remain connected through ordinary shell and extension crashes; compositor crashes are reported as session failures.
- Lock failure never exposes application surfaces or redirects input to them.
- Capture and remote input use portal consent and compositor-validated authorization.
- Keyboard and screen-reader journeys, IME, RTL, large text, reduced motion, and localization are tested end to end.
- Same-hardware GNOME comparison reports startup, idle CPU, whole-session PSS, input latency, frame pacing, GPU memory, and soak growth. Targets are based on a pinned baseline and reproducible traces.
- Recovery from a failed shell/config/package upgrade reaches a login-capable session without deleting user data.

## Decisions still open

Decided 2026-10-01: the parity baseline is **GNOME 51 as shipped in the TunaOS Marlin GNOME image**, and the first supported distribution is **TunaOS Marlin** (Arch Linux base, bootc image, x86_64). Both are recorded in [ADR 0007](adr/0007-gnome51-marlin-baseline.md) and [the roadmap](roadmap.md).

Still open before a hardware or security implementation spek is approved: GPU matrix beyond the Marlin VM; same-UID process threat model and trusted-service credential binding; 1.0 tiling requirement; settings compatibility map against GNOME 51 schemas; portal/capture grant lifecycle; and packaging/rollback strategy on bootc.

Current backend reality: the compositor runs nested (winit) or as a DRM/KMS hardware session (libseat, udev, GBM/EGL, libinput), chosen automatically by whether a host display exists. The hardware path is proven on vkms in CI; real-GPU qualification and the VM lane are tracked on the roadmap board.
