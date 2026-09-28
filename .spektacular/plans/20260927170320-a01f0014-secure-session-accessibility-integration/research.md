---
created_date: "2026-09-27"
document_status: draft
---

# Research — secure session, accessibility, and system integration

**Status:** Draft; protocol and distro-specific security research pending.

## Questions

1. Exact ext-session-lock-v1 semantics and compositor behavior after client loss; test against protocol version used by supported toolkits.
2. PAM helper service model, secret handling, lockout policy, distribution integration, and narrow assertion transport.
3. Current xdg-desktop-portal ScreenCast/Screenshot flows, PipeWire node ownership, selection, revocation, and backend responsibilities.
4. Same-UID process threat assumptions under target distributions and viable credential binding for shell/portal/lock services.
5. AT-SPI behavior across toolkit candidates, Orca workflows, recovery UI, and locked session.
6. Existing settings/service APIs and semantic compatibility per supported distro.

## Evidence sources to pin

Use upstream protocol XML/documentation, xdg-desktop-portal docs and implementation tests, distribution PAM/session docs, GNOME accessibility docs, and direct test results. Record versions and distinguish normative protocol requirements from distro policy. No security decision is made by a search snippet.
