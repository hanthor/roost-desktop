---
created_date: "2026-09-27"
document_status: draft
---

# Test plan — extension broker and API

**Status:** Draft; run only if extension capability is implemented  
**Strategy:** [Cross-cutting test strategy](../../../docs/test-strategy.md)

## Cases and evidence

- Capability matrix: each allowed operation succeeds only with its grant; clipboard, pixels, keystrokes, window content, filesystem, network, capture, remote input, and raw compositor commands are denied by default.
- Adversarial input: spoofed identity, bad version, malformed/oversized/high-rate event, request replay, capability escalation, memory/CPU/fuel exhaustion.
- Faults: worker crash/hang, broker crash/restart, shell crash, revocation while active; verify compositor and shell remain responsive and worker is disabled with reason.
- UI quotas: oversized indicators and update floods are bounded; search providers cancel on new query and timeout without blocking input.
- OS sandbox: inspect effective permissions, not only requested policy; test no-network/no-filesystem defaults on each declared distro.
- If Wasmtime ships, test host imports, fuel/memory bounds, attempted filesystem/network access, and worker failure containment. Otherwise mark tests deferred.
