---
created_date: "2026-09-27"
document_status: draft
---

# Test plan — nested compositor and shell recovery

**Status:** Draft  
**Strategy:** [Cross-cutting test strategy](../../../docs/test-strategy.md)

## Cases and evidence

- **Unit/property:** geometry remains valid/visible; focus transitions reference mapped windows; IDs are not reused; revisions increase monotonically; gaps trigger full snapshot; version negotiation and size limits reject invalid inputs; activation tokens reject wrong-seat, wrong-purpose, expired, and replayed use.
- **Nested integration:** launch compositor and shell plus GTK app and terminal; verify surfaces map, input, focus, move/resize, workspace and overview selection.
- **Fault injection:** with three apps open, kill shell 100 times; assert compositor responsiveness and app connections remain alive on every run; verify restart bound and state snapshot restoration. Also test disconnect, hang, crash loop, malformed/oversized IPC, and revision gap.
- **Critical path:** stall shell/control reader and verify pointer motion, resize, focus, and frame scheduling continue without shell response.
- **Diagnostics/privacy:** verify restart/revision/error logs exist and do not include content, credentials, clipboard, or raw pixels.

Every failure retains compositor/shell revision, launch script, redacted logs, and state snapshot. The nested test does not prove production identity or hardware security.
