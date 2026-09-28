---
created_date: "2026-09-27"
document_status: draft
---

# Extension broker and public API

**Status:** Draft; optional post-core capability  
**Parent:** [Program architecture](../../docs/architecture.md)  
**Depends on:** 000 threat model; 001 stable shell control boundary; 002 extension use-case inventory; 004 trusted-service and consent model.

## Overview

Provide a narrow, versioned, revocable extension API for selected user value without loading third-party code in the compositor or shell host. Extensions run in separate processes through a broker. Begin with declarative indicators and asynchronous search providers; expand only when a use case fits explicit capabilities and resource limits. Existing GNOME Shell extension compatibility is out of scope.

## Requirements

- **R1. Capability model:** define per-installation, revocable grants such as read-workspace-summary, add-indicator, add-search-provider, and request-layout-change. Default grants exclude raw input, clipboard, capture, filesystem, network, window contents, and arbitrary control.
- **R2. Isolation:** one process per extension or an explicitly grouped package, supervised outside compositor/shell. Extension never receives the compositor's privileged Wayland socket or trusted shell API.
- **R3. Broker:** mediate all API calls, validate schema/version/capability, enforce message-size and update-rate limits, and return typed denials.
- **R4. Fault containment:** crash, timeout, oversized messages, memory/fuel exhaustion, or protocol violation disables that extension for the session, emits a user-visible diagnostic, and leaves shell/compositor responsive.
- **R5. Declarative UI:** indicator widgets are bounded declarative data rendered by shell host. Search providers are asynchronous, cancellable, rate-limited, and cannot block typing or rendering.
- **R6. Resource policy:** specify CPU/time budget, memory budget, message quotas, startup/timeout behavior, restart policy, and audit logging for each capability.
- **R7. Consent:** capture, remote input, and permission escalation use independent explicit consent and reviewed service paths; extension permissions alone are insufficient.
- **R8. Portable modules:** Wasmtime may be evaluated within an isolated worker only after host imports, filesystem/network permissions, fuel, memory, and crash behavior have security tests.
- **R9. Compatibility:** publish API versioning, migration/deprecation policy, sample extension, permission UI, and a clear statement that GNOME Shell extensions/private Mutter APIs are not compatible.

## Constraints

- No native plugin ABI in compositor or shell host.
- OS process boundary and host-call policy are the security boundary; WebAssembly guest memory isolation alone is insufficient.
- Network is denied by default; any future network permission is explicit, scoped, revocable, and tested.
- Extension calls never execute in the compositor input/frame critical path.

## Acceptance criteria

- **A1. Least authority:** default extension cannot observe raw keystrokes, clipboard, pixels, window content, or arbitrary files/network; capability tests cover every grant and denial.
- **A2. Spoofing/validation:** malformed API messages, false identity claims, stale activation requests, capability escalation, and oversized events are rejected.
- **A3. Crash containment:** repeated extension crash/timeout/resource exhaustion does not terminate or hang shell/compositor; broker disables it for the session and reports why.
- **A4. Revocation:** revoke capability while extension runs and verify broker/service access stops within documented bound.
- **A5. UI/resource limits:** declarative panel widget size/update quotas and search cancellation/latency are enforced.
- **A6. Distribution:** install, update, disable, remove, and permission-review flows are documented and tested.
- **A7. WASM gate:** if Wasmtime ships, host imports, memory/fuel limits, no-network default, and worker crash tests pass; otherwise Wasmtime remains explicitly deferred.

## Dependencies and risks

This is not needed for the first implementation or 1.0 daily-driver gate unless a product decision makes an extension use case essential. Capability design can become too restrictive or too broad; validate the initial surface against documented use cases before stabilizing a public API.
