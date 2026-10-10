# GTK accessibility preferences

The GTK shell consumes `org.gnome.desktop.a11y always-show-universal-access-status` live. Its accessibility indicator appears when requested or when any menu feature is enabled, matching [GNOME Shell 51's indicator](https://github.com/GNOME/gnome-shell/blob/51.0/js/ui/status/accessibility.js). The ten rows follow the reference order: High Contrast, Zoom, Large Text, Screen Reader, Screen Keyboard, Visual Alerts, Sticky Keys, Slow Keys, Bounce Keys, Mouse Keys. Boolean rows bind their GNOME settings bidirectionally; Large Text uses factor 1.25 when enabled and resets the schema default when disabled. Accessibility Settings launches `gnome-control-center universal-access`.

This implements menu settings and visibility. It does not implement missing magnification, on-screen keyboard, AccessX, or compositor visual alerts. Those remain separately recorded capability gaps; a toggle state alone proves no functional assistive feature. Screen Reader remains subject to the hardware-session consumer and genuine Orca qualification.

`org.gnome.desktop.a11y.interface keyboard-focus-visible-timeout` controls focus indication in shell GTK windows. Positive values are seconds, zero keeps keyboard focus visible, and negative uses the toolkit default. [GTK 4.23.3's window implementation](https://github.com/GNOME/gtk/blob/4.23.3/gtk/gtkwindow.c) supports the corresponding toolkit property. When that property is absent, a weak-window policy refreshes the old fixed timeout until the requested duration expires, preserves pointer-driven clearing, and observes newly created windows. The old [GTK 4.14.5 default](https://github.com/GNOME/gtk/blob/4.14.5/gtk/gtkwindow.c) is three seconds. Missing schemas/keys remain safe on older hosts. The shell policy does not change external applications' toolkit settings.

## Focused proof

`scripts/tuna-gtk-accessibility-proof GTK_TEST_EXECUTABLE SOURCE_COMMIT ARTIFACT_DIRECTORY` runs the ignored `accessibility::tests::live_menu_and_focus_settings` test using actual GTK widgets, settings callbacks, and timers. Each run owns a mode 0700 runtime directory, private session and accessibility buses, a free Xvfb display, and memory settings, and records binary/source identity plus GTK version. A persistent guard queries the private bus for its daemon and registry processes, retains pidfds before the test starts, and verifies their exit before the runtime directory is removed. Query failure refuses proof. The dedicated focus schema fixture allows older hosts to exercise the new key without changing installed schemas or the user's settings.

Scope: source GTK widget behavior on the recorded host. Installed Wayland shell pixels, actual keyboard/pointer input, external toolkit clients, native output sessions, and paired GNOME behavior still need qualification. Pure policy tests and this focused source proof alone do not close P-A11Y-05.

### Local receipt (2026-10-10)

Exact implementation source `88aa154950603ca5d60efc0d678b5a349c557fcc` was compiled after a workspace package clean under the shared build-and-freeze guard, with one build job and development/test debug info disabled. The frozen test executable is `/tmp/tuna-v1-a11y-qualified-binaries/gtk-tests`, SHA256 `d4885c49ee1e61e0e9f938622af45aab276a977a64f71e2c0b6b799af1827cf7`.

- All nonignored GTK unit tests: **122 passed**, two graphical tests deliberately ignored by this invocation. Log: `/tmp/tuna-v1-a11y-qualified-unit-tests.log`.
- Actual graphical accessibility test: **1 passed** in 18.82 seconds. Artifacts: `/tmp/tuna-v1-a11y-proof-qualified`. Checks include each available boolean row's setting-to-widget and widget-to-setting directions, indicator visibility, ten-row structure, Large Text reset/1.25, one-second/default/forever focus, existing visible windows, inherited transient visibility, and pointer-style focus clearing.
- Strict GTK lint: `cargo clippy -p tuna-shell-gtk --all-targets -- -D warnings` passed. Log: `/tmp/tuna-v1-a11y-clippy.log`.
- Shellcheck, shell syntax, Rust formatting, and the 429-key settings inventory check passed.

The manifest records host GTK **4.14.5**, private display/session/accessibility bus identities and mode 0700 runtime. `owned-accessibility.json` confirms retained pidfds and verified exit for the private registry and bus daemon; the runtime directory was removed. The newer native GTK property branch is source-reviewed and compiled but was not exercised on this host. The remaining qualification scope above applies; this is a partial P-A11Y-05 receipt.
