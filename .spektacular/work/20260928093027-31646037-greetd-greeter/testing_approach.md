# Testing Approach (body)

- Unit tests on the prompt state machine (all prompt shapes, failure
  collapsing, cancel paths) and session-file parsing (fixtures,
  malformed entries, RWD default) — the most covered components,
  because auth UX bugs are the costliest here.
- Contract tests against the fake daemon: full exchanges (success,
  bad password, multi-prompt, session crash) asserting the model
  transitions and never a shell drop.
- Spec success metrics become behavioral tests where headless-safe
  (sign-in flow, picker contents, failure notice) and flagged manual
  VM checks where not (cold boot, real PAM).
- Follows the house loop: `cargo fmt`, `clippy -D warnings`,
  `cargo test`, enforced in CI; GTK UI code included in all three.
- Deliberate gap: no live-PAM or cold-boot tests in CI — unmockable
  secrets and hardware; covered by scripted VM runs at acceptance.
