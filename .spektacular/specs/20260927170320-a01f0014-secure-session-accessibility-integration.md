---
created_date: "2026-09-27"
document_status: draft
---

# Secure session, accessibility, and system integration

**Status:** Draft  
**Parent:** [Program architecture](../../docs/architecture.md)  
**Depends on:** 000 baseline and threat model; 001 compositor/control foundation; 002 shell workflows; 003 hardware session for production lock/capture journeys.

## Overview

Deliver the daily-driver trust and integration paths: secure lock/auth, portal-mediated capture, accessibility across shell and recovery UI, settings interoperability, notifications while locked, and reuse of existing desktop services. Security authority remains in the compositor and reviewed helpers; portals mediate user consent and never grant extensions ambient authority.

## Requirements

- **R1. Secure lock:** implement ext-session-lock-v1 semantics with compositor-owned lock latch. While locked, app surfaces are hidden and app input cannot receive events. Loss/crash of lock UI leaves session locked and triggers recovery or a safe session failure.
- **R2. Authentication:** delegate authentication to a reviewed PAM-facing helper; unlock is based on a narrowly scoped compositor-validated assertion. Secrets never enter logs, shell IPC, extension APIs, or ordinary app surfaces.
- **R3. Locked-session behavior:** test VT switching, suspend/resume, output hotplug, lock UI restart, notifications/privacy, capture revocation, and remote-input denial while locked.
- **R4. Portals/capture:** implement screen cast and screenshot through portal consent, source selection, PipeWire delivery, compositor-validated grants, and explicit revocation. Consent and authorization are distinct steps and the grant lifecycle is documented.
- **R5. Accessibility:** panel, overview, app grid, search, quick settings, notifications, lock, and recovery are keyboard and screen-reader operable via AT-SPI; define large-text, high-contrast, reduced-motion, magnification, on-screen keyboard, touch, and RTL test journeys.
- **R6. Settings:** publish compatibility map for theme/accent, wallpaper, input, display, power, notification, and shortcuts. Reuse existing keys only when semantics match; unsupported settings are documented and never silently misrepresented.
- **R7. Service integration:** use existing D-Bus/system services for accounts, power, network, audio, Bluetooth, media, notifications, and session lifecycle. Define owner, timeout, unavailable state, and restart policy for each integration.
- **R8. Trusted identity:** implement the reviewed same-UID and service-identity policy from the threat model for shell control, portal backend, lock UI, and privileged surfaces; do not trust app ID strings as identity.
- **R9. Failure behavior:** faults in the portal, lock UI, authentication helper, accessibility bus, or ordinary service cannot bypass lock/capture policy or block compositor input.

## Constraints

- No 1.0 release before lock, auth, portal consent/revocation, accessibility, and trusted identity gates pass.
- Compositor enforces lock/capture authorization; portal handles consent and user-facing source selection; auth helper returns a narrow assertion, not a general privileged channel.
- No extension permission authorizes capture, remote input, raw keystrokes, clipboard, or secrets.
- Reuse existing system services; do not rewrite network, audio, power, or the display manager.

## Acceptance criteria

- **A1. Lock fail-closed:** killing or disconnecting lock UI never exposes app pixels or routes input to app clients; authenticated recovery is required to unlock.
- **A2. Auth handling:** successful and failed PAM paths behave as specified; test logs, IPC, crash reports, and extension-visible state contain no secrets.
- **A3. Portal consent:** unauthorized capture is denied; accepted capture starts only after consent; revocation stops the stream; locked-session capture policy is enforced.
- **A4. Identity spoofing:** spoofed app IDs, same-UID unauthorized clients, stale assertions, and untrusted layer surfaces cannot obtain control, lock, or capture authority.
- **A5. Accessibility:** end-to-end keyboard and assistive-technology test journeys pass for all named shell/recovery surfaces, with remaining deviations documented and release-blocking severity assigned.
- **A6. Settings/service behavior:** compatibility map is published; unavailable services/timeouts are visible and recoverable; no operation blocks compositor input.
- **A7. Fault matrix:** portal/backend/auth/UI crashes, stalls, restart, suspend/resume, and hotplug leave lock/capture policy fail-closed.

## Dependencies and risks

Requires resolved threat model and service credential binding from spec 000, stable shell journeys from 002, and hardware/seat lifecycle from 003. PAM, portal, AT-SPI, and distribution integration each need specialist review. The lock path is a release blocker, not an optional prototype feature.
