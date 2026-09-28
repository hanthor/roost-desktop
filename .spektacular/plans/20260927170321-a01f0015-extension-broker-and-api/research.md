---
created_date: "2026-09-27"
document_status: draft
---

# Research — extension broker and API

**Status:** Draft; API and OS sandbox research pending.

## Questions

- Which extension use cases recur across target users and can fit a small capability set?
- What per-distro mechanisms enforce no-network/no-filesystem or scoped access for user services?
- How can process identity, installation identity, package update, and permission revocation be made auditable?
- Which broker IPC transport supports bounded messages, fd policy, cancellation, and resource accounting?
- Does Wasmtime add value after worker process controls, host imports, fuel/memory limits, and crash paths are accounted for?
- How will the API compatibility suite cover at least two released minor versions?

## Evidence

Review official OS sandbox interfaces and Wasmtime primary docs for candidate platform/version; prototype hostile workers and measure overhead. Compare user needs against the current GNOME extension ecosystem only to identify use cases, not promise compatibility.
