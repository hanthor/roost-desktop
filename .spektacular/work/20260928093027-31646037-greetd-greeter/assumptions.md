# Assumptions log

- greetd 0.10.3 pinned for protocol/config reference; daemon never vendored.
- No live PAM/cold-boot in CI; fake greetd server + fixtures in tests, VM/manual for A-criteria.
- greetd JSON-IPC wire details verified at implementation from the protocol doc, not memory.
- GTK4 choice covers the greeter surface only; shell toolkit (002 gate) stays open.
