---
created_date: "2026-09-27"
document_status: draft
---

# Research — shell workflow parity

**Status:** Draft; baseline research is pending.

## Questions

1. Which exact GNOME release/configuration defines each workflow and expected keyboard/gesture behavior?
2. Which system API owns each indicator/control and its error/disconnected states?
3. What app metadata/favorites/search provider sources can be reused without embedding extension code?
4. How do AT-SPI semantics and announcements work for overview transitions, dynamic results, and notifications in the selected toolkit?
5. Which notification server owns persistence, actions, DND, calendar, and lock redaction on supported distros?

## Evidence to collect

Use current GNOME HIG and official developer docs, test current pinned GNOME directly, and record direct upstream issue/MR state. The search-provider model and extension docs are leads, not proof of all current behavior. Store journey captures and discrepancy entries with baseline version.

- GNOME accessibility: https://developer.gnome.org/documentation/guidelines/accessibility.html
- GNOME search provider docs: https://developer.gnome.org/documentation/tutorials/search-provider.html
- GNOME extension architecture context: https://extensions.gnome.org/about/
