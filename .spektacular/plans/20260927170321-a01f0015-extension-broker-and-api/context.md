---
created_date: "2026-09-27"
document_status: draft
---

# Context — extension broker and API

**Status:** Draft; this is not an initial-delivery dependency.

## Intended security boundary

Extensions are untrusted separate processes. Broker validates capability and resource policy. Shell renders bounded declarative data. Compositor never loads extension code or exposes privileged globals to extensions. OS process/sandbox policy and broker host calls define authority; WebAssembly alone is insufficient.

## Dependencies

Spec 000 threat model; spec 001 trusted control boundary and lifecycle; spec 002 extension use-case inventory and search/indicator UI; spec 004 identity, consent, and accessibility rules.

## Non-goals

No GNOME Shell extension or private Mutter API compatibility. No default raw input, clipboard, pixels, window contents, filesystem, or network. Public extension API is optional and should follow core product evidence.

## Release relationship

Not required for the first daily-driver release unless its scope is explicitly changed through an ADR and the capability/security gates are met.
