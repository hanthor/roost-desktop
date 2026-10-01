---
tags: [dconf, gsettings, settings]
---

# dconf interop is an open 004/R9 item

GTK apps read their own settings via GSettings/dconf in any session,
but desktop-level `org.gnome.desktop.*` schemas (font, theme, idle
delay, keybindings) have no RWD counterpart yet. The R9
settings-compatibility map must decide: shim schemas, documented
divergence, or a translating settings daemon. Shell and toolkit spikes
must not hardcode font/theme/scale sources before this is decided.
Tracked as question 7 in the 004 plan research.
