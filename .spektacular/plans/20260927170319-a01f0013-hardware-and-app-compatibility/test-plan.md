---
created_date: "2026-09-27"
document_status: draft
---

# Test plan — hardware session and app compatibility

**Status:** Draft  
**Strategy:** [Cross-cutting test strategy](../../../docs/test-strategy.md)

## Cases and evidence

- Matrix each supported distro/GPU/driver/output config; test startup, VT switch, output hotplug, primary change, suspend/resume, GPU reset, and teardown.
- GTK, Qt, native Wayland, and XWayland clients: map, focus, resize, fullscreen, transient dialogs, clipboard, DnD, layouts, IME, and relevant device input.
- Native and XWayland scaling at 100/125/150/200% where supported; assert geometry and pointer/touch coordinates align before/after output hotplug.
- Mixed-resolution/scale and mixed 60/120 Hz presentation; document measured frame/presentation behavior and capability fallbacks. Test VRR only on declared capable hardware.
- Probe DMA-BUF, explicit sync, presentation feedback, hardware cursor, direct scan-out, and fallback; compare visible correctness/capture constraints.
- Retain system/hardware manifest, protocol probe outputs, client versions, recordings, logs, and raw timing traces.
