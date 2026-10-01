# Architecture decision records

Record decisions here using one file per decision, with status, context, decision, alternatives, consequences, and evidence. Do not treat prototype convenience as a production decision.

## Decision register

These decisions are open. Record the decision date, owner, options/evidence, consequences, and dependent speks in an individual ADR. A decision marked “blocking” must be resolved before that dependent work passes its gate.

| Decision | Needed by | Status |
|---|---|---|
| Pinned GNOME release/configuration and reference workload | 000 baseline; 002 parity; 006 release | Open; blocks parity/performance claims |
| Supported distro, kernel, GPU/driver, output and device matrix | 003 hardware; 006 release | Open; blocks support claims |
| Nested backend and pinned Smithay/protocol revisions | 001 implementation | Provisionally decided for nested preview: [0001](0001-nested-backend-smithay-pin.md) (decided), [0002](0002-shell-control-ipc-envelope.md) + [0003](0003-nested-supervision-and-recovery.md) (proposed, for plan Phase 0 review) |
| Same-UID threat model and trusted shell/lock/portal identity | 004 security; 005 extensions | Control socket decided ([0005](0005-control-socket-peer-authentication.md)); lock/portal identity open, production release blocker |
| UI toolkit based on overview/layer-surface/AT-SPI/IME/RTL/resource evidence | 002 implementation | Decided: GTK4 + libadwaita ([0006](0006-gtk4-libadwaita-shell.md)) |
| Production lock/auth and portal/capture grant lifecycle | 004 implementation | Open; 1.0 release blocker |
| Settings compatibility map and notification/service ownership | 002/004 | Open; blocks truthful integration behavior |
| Tiling requirement for 1.0 | Release scope review | Proposed later; promote only by evidence-backed scope change |
| Package/session upgrade rollback per distro | 006 release | Open; blocks daily-driver release |
| Monorepo vs split-off repos | All | Decided: monorepo, preserve seams ([004](0004-no-split-preserve-seams.md)) |
