# Milestones (body)

## M1 — Headless login conversation works
The greeter completes full auth exchanges against the fake daemon:
user pick, password and multi-factor prompts, success handoff, and
generic failure — all proven by contract tests with no display.
Validation: fake-daemon suite green, including bad-password and
crashed-session cases.

## M2 — Session picker lists real sessions
Installed session files appear in the picker with RWD default, parsed
from fixtures in tests and system paths at runtime. Validation:
fixture tests plus a manual run listing this machine's sessions.

## M3 — Graphical login screen
The GTK login window renders users, prompts, picker, and notices from
the proven model, keyboard-navigable end to end. Validation: scripted
walkthrough against the fake daemon under a nested compositor, plus
the HIG pass from the gnome-gui skill.
