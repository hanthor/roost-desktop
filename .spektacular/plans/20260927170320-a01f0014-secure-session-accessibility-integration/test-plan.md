---
created_date: "2026-09-27"
document_status: draft
---

# Test plan — secure session, accessibility, and system integration

**Status:** Draft  
**Strategy:** [Cross-cutting test strategy](../../../docs/test-strategy.md)

## Security and fault cases

- Lock, kill lock UI, crash auth helper, stall helper, VT switch, suspend/resume, hotplug, portal crash, and service restart. Assert compositor lock latch persists, app pixels stay hidden, and app input receives nothing until valid unlock.
- Exercise valid/invalid/expired/replayed unlock assertions, spoofed app IDs/process claims, unauthorized privileged layers, and same-UID untrusted processes.
- Deny capture without consent; start after consent; revoke while active; assert capture stream ends within the documented bound. Test locked capture and notification privacy policy.
- Search logs, IPC, crash artifacts, shell/extension view, and core dumps for auth secrets/tokens and unauthorized image/clipboard data.

## Accessibility and integration cases

- Screen reader + keyboard journeys across panel, overview, grid/search, quick settings, notifications, lock, and recovery; cover high contrast, large text, reduced motion, RTL/locales, touch, IME, on-screen keyboard, and magnification approach.
- Exercise each mapped setting/service supported, unsupported, unavailable, timed out, and restarted; verify truthful state and bounded response.
- Retain review signoff, test environment/toolkit/AT versions, portal grant logs, lock-state assertions, and redacted traces.
