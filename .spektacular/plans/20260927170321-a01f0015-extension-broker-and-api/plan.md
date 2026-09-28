---
created_date: "2026-09-27"
document_status: draft
---

# Plan — extension broker and API

**Status:** Draft plan; optional post-core work  
**Spec:** `extension-broker-and-api`

## Phase 1 — use cases and capability review

- Inventory common GNOME extension use cases as user needs without assuming source/API compatibility.
- Select first supported use cases that fit bounded declarative indicators or isolated asynchronous search.
- Threat-model broker, package identity, per-installation grants, revocation, and OS sandbox profile.

**Gate:** initial capability list and default-deny policy reviewed before implementation.

## Phase 2 — broker and worker

- Implement versioned bounded messages, identity/permission checks, quotas, timeouts, logs, and user-visible disable reason.
- Launch each extension in a separate supervised process; enforce network/filesystem defaults at OS level.
- Add declarative indicator renderer and cancellable search-provider adapter.

## Phase 3 — adversarial/resource tests

- Test spoofing, malformed/oversized/rate-exhausting traffic, capability escalation, crashes, hangs, memory/CPU limits, revocation, shell restart, and broker restart.
- Evaluate Wasmtime only if process worker host-call policy, fuel, memory, and import tests are available.

## Phase 4 — SDK and distribution

- Publish API versioning/deprecation policy, sample extension, permission UI, install/update/remove flow, and troubleshooting.
- Run compatibility suite across at least two released minor API versions before claiming stable public API.

**Exit gate:** broker failure cannot hang or terminate shell/compositor; grants are revocable; no unauthorized access case passes.
