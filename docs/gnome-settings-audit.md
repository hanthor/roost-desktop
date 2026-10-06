# GNOME 51 Settings and feature gap re-audit

Candidate: main `44edb6aca704ef0ada32b8c6111383ea5c3c6afe`, 2026-10-06. Reference: [GNOME control-center 51.0](https://github.com/GNOME/gnome-control-center/tree/6ae712c0454f2586f00821c3486150c1e8feb3e7), with panel C, Blueprint and GtkBuilder sources inspected. The [static item catalog](../tests/settings/gnome51-control-center-items.json) retains source locations; the [429-key desktop inventory](settings-desktop-inventory.md) is a separate input. Dynamic devices, apps, permissions and shortcuts require runtime enumeration. Neither inventory is a behavior pass.

Merged follow-ups through main `95490e3` add finite bounded Activities triggers (#369), GNOME enum reduced-motion handling (#370), truthful rejection of unsupported display mode/disable requests (#371), and strict native VM lifecycle qualification (#265). The source-pinned catalog remains the original audit input; those fixes do not close their broader capability or published-image acceptance issues.

**The desktop does not yet work fully with GNOME Settings.** Some controls work live, some only store preferences, some have no consumer, and service-backed controls lack shipped-image acceptance. Nested tests do not certify physical peripherals, multi-output GPU behavior or the final image.

Status: **live** means a source consumer exists (test scope is given in settings-map); **missing/partial** means a confirmed implementation limitation; **qualify** means an external service or runtime path requires evidence, not that the service is known absent.

[Central GitHub tracker](https://github.com/tuna-os/tuna-desktop/issues/367) contains all capability and existing production acceptance issues.

## Panel controls and responsible paths

| Panel | Controls / behavior | Current mapping | Tracking |
|---|---|---|---|
| Appearance / Background | Default/dark style, accent, add/select picture; placement and dynamic backgrounds | Style/accent via GTK, wallpaper URIs live; placement/dynamic metadata partial | [#360](https://github.com/tuna-os/tuna-desktop/issues/360) |
| Displays | Scale, arrangement, primary; resolution, refresh/VRR, join/mirror, orientation, enable/disable, HDR, adjust for TV | Scale/position/primary live; modes/rotation/mirror partial; HDR/color missing; VRR/TV qualification | [#340](https://github.com/tuna-os/tuna-desktop/issues/340), [#341](https://github.com/tuna-os/tuna-desktop/issues/341), [#345](https://github.com/tuna-os/tuna-desktop/issues/345), [#364](https://github.com/tuna-os/tuna-desktop/issues/364) |
| Night Light | Enable, restart filter, schedule, from/to, temperature | Stored only; capability false; no color transform | [#339](https://github.com/tuna-os/tuna-desktop/issues/339) |
| Mouse & Touchpad | Left/right primary, mouse speed/acceleration/natural scroll; touchpad enable, typing/mouse disable, click method, tap, speed, scroll method/direction; pointing stick | Speed/natural scroll/tap/typing disable subset live; remaining native settings missing | [#343](https://github.com/tuna-os/tuna-desktop/issues/343) |
| Keyboard | Add/reorder/remove input source; shared/per-window, alternate/compose key; repeat; overview and custom/default shortcuts | Sources/options/repeat and many shell grabs live; per-window/Num Lock/custom service and remaining actions incomplete | [#346](https://github.com/tuna-os/tuna-desktop/issues/346), [#347](https://github.com/tuna-os/tuna-desktop/issues/347) |
| Multitasking | Hot corner, edge resize, dynamic/fixed/count, primary/all-display workspaces, app-switch inclusion; reopen windows | Corner enable and coordinate bounds live; output placement/pressure gaps; other policies ignored; restore missing | [#336](https://github.com/tuna-os/tuna-desktop/issues/336), [#337](https://github.com/tuna-os/tuna-desktop/issues/337), [#338](https://github.com/tuna-os/tuna-desktop/issues/338) |
| Notifications | DND, global lock notifications, per-app enable/sound/banner/details/lock/details | Global DND live; per-app and lock privacy preferences ignored | [#348](https://github.com/tuna-os/tuna-desktop/issues/348) |
| Accessibility: Seeing | Reader/configuration, contrast, status shapes, motion, text/cursor size, sound keys, scrollbars, focus visibility | Some GTK keys and GNOME reduced-motion enum / shell animation control work; compositor cursor and modern preferences missing; spoken reader unqualified | [#342](https://github.com/tuna-os/tuna-desktop/issues/342), [#351](https://github.com/tuna-os/tuna-desktop/issues/351), [#352](https://github.com/tuna-os/tuna-desktop/issues/352) |
| Accessibility: Hearing | Overamplification, visual alerts, flash area/test | Amplification capped; visual/audible bell policies missing | [#353](https://github.com/tuna-os/tuna-desktop/issues/353) |
| Accessibility: Typing | Screen keyboard, keyboard enable shortcuts, caret blink/speed, repeat/delay, sticky/slow/bounce keys and beeps | GTK caret and native repeat subset; screen keyboard and seat aids missing | [#349](https://github.com/tuna-os/tuna-desktop/issues/349), [#350](https://github.com/tuna-os/tuna-desktop/issues/350) |
| Accessibility: Pointing & Clicking | Mouse Keys, locate pointer, hover focus, double-click delay, simulated secondary and hover click | Toolkit double-click translation separate from compositor; seat aids and hover policy missing | [#342](https://github.com/tuna-os/tuna-desktop/issues/342), [#347](https://github.com/tuna-os/tuna-desktop/issues/347), [#350](https://github.com/tuna-os/tuna-desktop/issues/350) |
| Accessibility: Zoom | Enable, factor/lens/screen/follow, extend edges, crosshair color/size/overlap, inversion/brightness/contrast/color | No compositor magnifier | [#349](https://github.com/tuna-os/tuna-desktop/issues/349) |
| Wellbeing | History enable, daily limit, grayscale, eyesight/movement reminders/schedule/sounds | No policy/history/reminder consumers | [#354](https://github.com/tuna-os/tuna-desktop/issues/354) |
| Power | Power mode/button, charge max/health, battery percentage, ambient/dim/blank, automatic saver/suspend battery/AC/delay | Percentage and native backlight live; GNOME brightness interface missing; service/hardware policy qualification | [#355](https://github.com/tuna-os/tuna-desktop/issues/355) |
| Privacy: Screen Lock | Blank delay, lock enable/delay, lock notifications, USB protection, restrict viewing angle | Idle/lock enable/delay live; notification policy and hardware security integration incomplete | [#348](https://github.com/tuna-os/tuna-desktop/issues/348), [#356](https://github.com/tuna-os/tuna-desktop/issues/356) |
| Privacy: Permissions / Data / Security | Location/camera access, app grants, history duration/clear, trash/temp cleanup/age, diagnostics, Thunderbolt grants, device-security report | Toolkit recent-file translation exists; external enforcing services and security controls need qualification | [#356](https://github.com/tuna-os/tuna-desktop/issues/356), [#357](https://github.com/tuna-os/tuna-desktop/issues/357) |
| Sound | Output/input/test/profiles/volumes, balance/fade/subwoofer, per-app levels, alert choice | Native PipeWire media controls live; full Settings mixer/profile paths unqualified; sound/bell/amplification gaps | [#353](https://github.com/tuna-os/tuna-desktop/issues/353) |
| Search | App providers enable/reorder, filesystem/default/bookmark/custom locations | SearchProvider2 settings live; filesystem indexing/location service qualification | [#361](https://github.com/tuna-os/tuna-desktop/issues/361) |
| Apps | Search/notifications/background/screenshot/wallpaper/sound/camera/mic/location permissions; global shortcuts; file/link handlers; storage/cache; defaults; removable-media autostart | Several portal/launch paths exist; full enforcement and media handling unqualified | [#348](https://github.com/tuna-os/tuna-desktop/issues/348), [#361](https://github.com/tuna-os/tuna-desktop/issues/361), [#365](https://github.com/tuna-os/tuna-desktop/issues/365) |
| Wi-Fi / Network | Airplane, hidden/saved networks, hotspot/share QR; wired MAC/MTU/802.1x; IPv4/6 addresses/routes/DNS/search domains; proxy; VPN/WireGuard peers | NetworkManager-backed; shell connectivity is not full Settings proof | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| Bluetooth | Enable, airplane/hardware-off states, pairing/device operations | BlueZ/rfkill-backed; physical qualification required | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| WWAN | SIM/slot, PIN, mobile data/roaming, network/mode, APN, modem details | ModemManager-backed; hardware qualification required | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| Online Accounts | Add/remove/authenticate, enabled account services | GOA and consumer-app integration; qualification required | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| Printers | Add/remove/default/details, location/driver/PPD/options, test, jobs/authentication/clear, cleaning/ink/status | CUPS-backed; virtual-printer qualification required | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| Sharing | Device name, file sharing/address/password, media sharing/folders/networks | hostname and sharing-daemon integration; qualification required | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| System: Region & Language / Users | User/login-screen language/formats, logout; add/remove/enterprise user, name/password/admin/fingerprint/autologin/parental control | AccountsService/PAM/greetd/locale/fprintd/realmd integration; qualification required; [interactive auth gap](https://github.com/tuna-os/tuna-desktop/issues/366) | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| System: Date & Time | NTP/manual time/timezone, auto timezone, 12/24h, first day, weekday/date/seconds/week numbers | Clock and calendar preferences have live consumers; calendar G-CAL-PREFS qualification and shipped timedate/Geoclue qualification remain required | [#359](https://github.com/tuna-os/tuna-desktop/issues/359) |
| System: Remote Desktop | Sharing/control, hostname/port/user/password/generation/fingerprint, remote login | Portal keyboard/pointer supported; actual RDP daemon and login unqualified, touch/clipboard capability gaps | [#358](https://github.com/tuna-os/tuna-desktop/issues/358) |
| System: SSH / About / Updates | SSH switch/login command, device name, OS/build/shell/hardware report, software updates | System services and immutable-image integration; require truthful identity and successful update/rollback qualification | [#362](https://github.com/tuna-os/tuna-desktop/issues/362) |
| Wacom | Pen mode/handedness/aspect/calibration, output mapping, stylus buttons/pressure, pad keystrokes/test | Tablet protocol and event routing missing | [#344](https://github.com/tuna-os/tuna-desktop/issues/344) |
| Screenshot UI / native protocols | Selection keys, tab dragging, GTK3 portal parents, version floors, color/effects, explicit sync/DRM lease | Screenshot keys and recorded protocol deviations remain | [#363](https://github.com/tuna-os/tuna-desktop/issues/363), [#364](https://github.com/tuna-os/tuna-desktop/issues/364) |

## Filed capability gaps

Every issue identifies source evidence, affected controls, expected behavior and acceptance. Qualification issues explicitly avoid claiming an external service is absent.

| Issue | Gap |
|---|---|
| [#336](https://github.com/tuna-os/tuna-desktop/issues/336) | Bound hot-corner coordinates and implement output-aware activation |
| [#337](https://github.com/tuna-os/tuna-desktop/issues/337) | Honor Multitasking workspace, edge-tiling and app-switcher preferences |
| [#338](https://github.com/tuna-os/tuna-desktop/issues/338) | Implement GNOME 51 session save and restore |
| [#339](https://github.com/tuna-os/tuna-desktop/issues/339) | Implement Night Light rather than only writing its setting |
| [#340](https://github.com/tuna-os/tuna-desktop/issues/340) | Implement display modes, refresh rate, rotation, mirroring and output disabling |
| [#341](https://github.com/tuna-os/tuna-desktop/issues/341) | Implement ICC, HDR and GNOME color management |
| [#342](https://github.com/tuna-os/tuna-desktop/issues/342) | Implement compositor cursor themes, sizes, shapes and locate-pointer |
| [#343](https://github.com/tuna-os/tuna-desktop/issues/343) | Honor mouse, touchpad, pointing-stick and trackball settings |
| [#344](https://github.com/tuna-os/tuna-desktop/issues/344) | Route tablet input and implement GNOME Wacom settings |
| [#345](https://github.com/tuna-os/tuna-desktop/issues/345) | Implement touchscreens, output mapping and sensor-driven orientation |
| [#346](https://github.com/tuna-os/tuna-desktop/issues/346) | Honor keyboard input-source policy, Num Lock and custom shortcuts |
| [#347](https://github.com/tuna-os/tuna-desktop/issues/347) | Complete the GNOME window-manager shortcut and focus policy surface |
| [#348](https://github.com/tuna-os/tuna-desktop/issues/348) | Honor per-app notification preferences and lock-screen privacy |
| [#349](https://github.com/tuna-os/tuna-desktop/issues/349) | Implement magnification and on-screen keyboard accessibility |
| [#350](https://github.com/tuna-os/tuna-desktop/issues/350) | Implement accessibility keyboard and pointer input aids |
| [#351](https://github.com/tuna-os/tuna-desktop/issues/351) | Honor reduced motion, focus visibility and accessibility-menu preferences |
| [#352](https://github.com/tuna-os/tuna-desktop/issues/352) | Qualify Orca screen-reader activation and complete spoken shell navigation |
| [#353](https://github.com/tuna-os/tuna-desktop/issues/353) | Honor visual/audible alerts, sound preferences and overamplification |
| [#354](https://github.com/tuna-os/tuna-desktop/issues/354) | Implement Wellbeing screen-time recording, limits and break reminders |
| [#355](https://github.com/tuna-os/tuna-desktop/issues/355) | Implement GNOME 51 brightness interface and qualify power policies |
| [#356](https://github.com/tuna-os/tuna-desktop/issues/356) | Enforce GNOME privacy device controls and qualify retention/security services |
| [#357](https://github.com/tuna-os/tuna-desktop/issues/357) | Enforce or explicitly delimit GNOME administrative lockdown policies |
| [#358](https://github.com/tuna-os/tuna-desktop/issues/358) | Qualify GNOME Settings RDP sharing and remote login end to end |
| [#359](https://github.com/tuna-os/tuna-desktop/issues/359) | Honor GNOME calendar week numbering and first-day preferences |
| [#360](https://github.com/tuna-os/tuna-desktop/issues/360) | Complete background placement and dynamic wallpaper compatibility |
| [#361](https://github.com/tuna-os/tuna-desktop/issues/361) | Qualify Apps permissions, global shortcuts, default handlers and removable media |
| [#362](https://github.com/tuna-os/tuna-desktop/issues/362) | Qualify service-backed GNOME Settings panels on the shipped Marlin image |
| [#363](https://github.com/tuna-os/tuna-desktop/issues/363) | Complete GNOME 51 screenshot selection keyboard controls |
| [#364](https://github.com/tuna-os/tuna-desktop/issues/364) | Close remaining Mutter protocol and native GPU compatibility deviations |
| [#365](https://github.com/tuna-os/tuna-desktop/issues/365) | Reconcile legacy and app-owned GNOME settings without false parity claims |
| [#366](https://github.com/tuna-os/tuna-desktop/issues/366) | Support and qualify interactive authentication beyond one password |

## Existing production acceptance blockers

Lifecycle/suspend/VT/logout qualification remains in #62/#68; same-image performance and enforceable budgets in #73/#315; broader stress and final 24-hour soak in #203; test-strategy/security requirements in #8; physical hardware matrix in #314; release support commitments in #313. The strict native VM lane for merged PR #265 now passes lock repaint, stale successful PAM rejection after S3, fresh unlock, client repaint and painted greeter logout. Refreshed PR #288 is testing that foundation with its single-suspend performance fixture. The published-nightly lane for #273 previously failed final PAM unlock; its refreshed observer run must still qualify the actual shipped payload. Native preview evidence and passing initial logins do not establish final published-image acceptance.

The six existing switch-windows/cycle-windows/cycle-group bindings are corrected in the key inventory. Deprecated GTK, legacy screensaver and app-owned settings are tracked as ownership/compatibility decisions, not assumed GNOME 51 Settings regressions. GNOME Shell JavaScript extensions remain a separate compatibility difference; advertised Wayland globals do not imply complete behavior.

Acceptance should enumerate dynamic controls on the final shipped image, save per-control changed-behavior and negative-control results with exact source/package/image identities, and restore settings afterward. VM evidence covers virtual devices; GPU, radio, biometrics, touch/tablet and multiple physical monitors require separate evidence.
