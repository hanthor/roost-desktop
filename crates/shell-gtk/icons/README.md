# GNOME Shell's own icons

These symbolic icons are copied unchanged from GNOME Shell 51.0
(`data/icons/scalable/actions` and `data/icons/scalable/status`,
https://gitlab.gnome.org/GNOME/gnome-shell, GPL-2.0-or-later). GNOME Shell
bundles them rather than taking them from the icon theme. Examples are
`dark-mode-symbolic`, `screenshooter-symbolic` and
`ornament-check-symbolic`. Roost bundles them the same way, compiled into
the shell as a GResource (`build.rs`) and added to the icon theme at
startup, so its toggles and menus show the icons GNOME shows.
