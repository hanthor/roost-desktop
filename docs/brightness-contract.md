# GNOME 51 brightness adapter scope

The GTK shell exports `org.gnome.Shell.Brightness` at
`/org/gnome/Shell/Brightness`: readonly boolean `HasBrightnessControl`, void
`SetDimming(b)` and `SetAutoBrightnessTarget(d)`, and the payloadless
`BrightnessChanged` user-update signal. This candidate still requires fresh
Rust, GTK and native VM qualification; source and controlled fixtures do not
qualify physical backlight hardware.

Positive capability requires a bounded readable kernel backlight associated by
its actual canonical DRM connector ancestry with a currently compositor-tracked
output, plus the actual system bus. The lit-output inventory does not establish
display power-save support. Unassociated legacy ACPI backlights retain the
existing global helper; they do not establish this interface's capability.
A VM with an actually empty kernel inventory must report false. Harmless dimming
disable and negative automatic-target reset succeed without eligible hardware.
Positive unsupported operations fail. Nonfinite targets are explicitly refused
by this bounded adapter; upstream JavaScript does not define the same explicit
error contract.

The associated controller shares user intent across UI, keys and policy calls.
Normalized zero maps to `max(1, max_brightness / 100)`, then values interpolate
through the remaining hardware range. Global controls preserve monitor ratios,
including when all values are zero. Dimming clips the effective level to the
live installed `idle-brightness` setting. Automatic brightness uses the GNOME
user-bias formula. Disabling either policy restores the user baseline after
exact actual readback. Policy changes do not emit a user-update signal.
External hardware changes cancel dimming before subsequent policy derivation.

Each hardware transaction uses asynchronous logind calls with a two-second
call bound and exact device, range, output-membership and post-call readback
checks. Partial writes and refusals return failure; requested percentages are
never presented as observed values. Concurrent user requests are refused while
an owned transaction is pending; live idle-key changes coalesce to the latest
value and apply after completion. Individual backlight scalar reads and the
inventory are bounded; rejected inventory preserves the user baseline while
withholding capability. This does not claim blocking filesystem metadata calls
have a real-time deadline.

`G-BRIGHTNESS-CONTRACT` exercises the real session-bus adapter against an
explicitly synthetic associated sysfs/logind fixture: user baseline, dim/restore,
automatic bias/reset, live idle-key mutation/restoration, refused writes, exact
readback mismatch, nonfinite rejection and no policy user-update signals.
`V-BRIGHTNESS-CONTRACT` uses the actual native VM kernel inventory and original
shell owner PID/UID/start identity, requiring empty inventory, false capability,
unsupported positive operations and successful harmless resets. Neither proves
real panel luminance or physical logind permissions.

Issue #355 remains open for physical hardware, connector association on other
backlight topologies, persistence across shell restart, cloned-monitor policy,
ambient sensor integration, actual blank/display power-save, idle/suspend and
battery/AC policy, external service and shipped-image qualification.

Primary contracts and formulas: GNOME Shell 51.0
[`org.gnome.Shell.Brightness.xml`](https://github.com/GNOME/gnome-shell/blob/51.0/data/dbus-interfaces/org.gnome.Shell.Brightness.xml),
[`brightnessManager.js`](https://github.com/GNOME/gnome-shell/blob/51.0/js/misc/brightnessManager.js),
[`shellDBus.js`](https://github.com/GNOME/gnome-shell/blob/51.0/js/ui/shellDBus.js),
GNOME Settings 51.0
[`cc-power-panel.c`](https://github.com/GNOME/gnome-control-center/blob/51.0/panels/power/cc-power-panel.c),
and GNOME Settings Daemon 51.0
[`gsd-power-manager.c`](https://github.com/GNOME/gnome-settings-daemon/blob/51.0/plugins/power/gsd-power-manager.c).
