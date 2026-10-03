# Walkthrough

Roost's features as they look today. Every frame is taken by the GTK
shell proof (`scripts/roost-gtk-shell-proof`) in a nested session, the
same run CI makes on every change, so the windows are the proof's test
windows and the services are its stubs. Regenerate with
`scripts/roost-walkthrough regen`; the list of features lives in
`docs/walkthrough.tsv`. How each feature compares with GNOME 51 is in
the [parity ledger](parity-ledger.md).

## Panel

### Top bar

GNOME 51's black top bar: Activities with the workspace dots, the centred clock, and the status icons that open quick settings. (`P-PN-01`)

![Top bar](walkthrough/panel.png)

### Date menu

The clock opens the notification list beside the calendar, with today's events from the calendar server underneath. (`P-PN-02`)

![Date menu](walkthrough/calendar.png)

### Tray menus

An AppIndicator (StatusNotifierItem) gets a panel icon, and its menu opens from the panel. (`P-TR-01`)

![Tray menus](walkthrough/tray.png)

## Quick settings

### Quick settings

Volume and brightness sliders over the toggle grid. Each toggle reads its daemon: NetworkManager, BlueZ, power-profiles-daemon and GSettings. (`P-PN-03`)

![Quick settings](walkthrough/quick-settings.png)

### Wi-Fi menu

Networks grouped and sorted as GNOME does, scanned while the menu stays open; picking a saved one activates it. (`P-PN-03`)

![Wi-Fi menu](walkthrough/wifi-menu.png)

### Wi-Fi password

A secured network NetworkManager has no secret for: the shell's secret agent asks with GNOME's dialog. (`P-PN-03`)

![Wi-Fi password](walkthrough/network-agent.png)

### Wired connections

The Wired toggle's menu: Disconnect for the active profile, and Wired Settings. (`P-PN-03`)

![Wired connections](walkthrough/wired-menu.png)

### Bluetooth menu

Paired devices with Connect or Disconnect, and Bluetooth Settings. (`P-PN-03`)

![Bluetooth menu](walkthrough/bluetooth-menu.png)

### Microphone

While an app records, an orange microphone leads the panel and a Microphone slider sits under the volume slider. (`P-PN-03`)

![Microphone](walkthrough/microphone.png)

### Power menu

Suspend, Restart…, Power Off… and Log Out…, open in place. (`P-SY-07`)

![Power menu](walkthrough/power-menu.png)

### Volume OSD

gnome-settings-daemon's volume keys show GNOME's on-screen display through ShowOSD. (`P-ST-01`)

![Volume OSD](walkthrough/osd-volume.png)

### Brightness OSD

The brightness key steps the backlight through logind and shows the level. (`P-ST-01`)

![Brightness OSD](walkthrough/osd-brightness.png)

## Windows

### Windows

libadwaita windows placed by Mutter's cascade; the focused one on top. (`P-WM-05`)

![Windows](walkthrough/windows.png)

### App popovers

A header-bar menu opens as an xdg popup at the place its positioner asks for. (`P-WM-06`)

![App popovers](walkthrough/popover.png)

### Edge tiling

Dragging a window to the left edge shows the tile preview over the left half. (`P-WM-02`)

![Edge tiling](walkthrough/tile-preview.png)

### Window menu

Right-clicking a header bar opens GNOME's window menu. (`P-WM-05`)

![Window menu](walkthrough/window-menu.png)

### X11 apps

XWayland starts with the session, and X11 apps get GNOME placement and focus. (`P-SY-04`)

![X11 apps](walkthrough/x11.png)

## Switcher

### Alt+Tab

One icon per app in most-recently-used order; releasing Alt focuses the highlighted one. (`P-WM-01`)

![Alt+Tab](walkthrough/switcher.png)

### Window thumbnails

An app with several windows shows them as thumbnails (Down, or Alt+Above_Tab). (`P-WM-01`)

![Window thumbnails](walkthrough/switcher-thumbnails.png)

## Overview

### Overview

Super opens the overview: live previews with app icons, the search entry and the dash. (`P-OV-01`)

![Overview](walkthrough/overview.png)

### Workspaces

With windows on more than one workspace, the thumbnail strip shows above the previews. (`P-OV-02`)

![Workspaces](walkthrough/workspaces.png)

### Moving windows

Dragging a preview onto the next workspace moves the window there. (`P-OV-02`)

![Moving windows](walkthrough/overview-drag.png)

### Workspace switcher

A workspace key outside the overview shows GNOME's workspace popup. (`P-WM-03`)

![Workspace switcher](walkthrough/workspace-popup.png)

## Search and apps

### Search

Typing in the overview searches apps; the top hit is Enter's target. (`P-OV-04`)

![Search](walkthrough/search.png)

### Search providers

GNOME Shell search providers over D-Bus add their own results. (`P-OV-04`)

![Search providers](walkthrough/search-provider.png)

### App grid

Show Apps opens the grid, with GNOME's app folders as tiles. (`P-OV-05`)

![App grid](walkthrough/app-grid.png)

### App folders

A folder opens GNOME's folder dialog with its apps. (`P-OV-05`)

![App folders](walkthrough/app-folder.png)

### Pages

More apps than fit page the grid, with indicators and arrows. (`P-OV-05`)

![Pages](walkthrough/app-grid-pages.png)

### Making folders

Dropping one app on another makes a folder of the two. (`P-OV-05`)

![Making folders](walkthrough/folder-made.png)

### Reordering

An app dropped on a tile's edge moves there, and the order is saved to app-picker-layout. (`P-OV-05`)

![Reordering](walkthrough/grid-reorder.png)

## Notifications

### Banners

A notification sent over org.freedesktop.Notifications shows a banner at the top. (`P-NT-01`)

![Banners](walkthrough/banner.png)

### Notification list

The banner then waits in the date menu's list. (`P-NT-02`)

![Notification list](walkthrough/notification-list.png)

### Do Not Disturb

Do Not Disturb holds banners back but keeps them in the list. (`P-NT-02`)

![Do Not Disturb](walkthrough/do-not-disturb.png)

## System

### Screenshot UI

Print opens GNOME's screenshot UI: area, screen or window. (`P-SY-05`)

![Screenshot UI](walkthrough/screenshot-ui.png)

### Authentication

polkit's requests show GNOME's authentication dialog.

![Authentication](walkthrough/polkit.png)

### End session

Power Off… opens GNOME's 60-second confirmation dialog. (`P-SY-07`)

![End session](walkthrough/end-session.png)

### Lock screen

Suspending locks the session; the curtain shows the clock. (`P-LK-01`)

![Lock screen](walkthrough/lock.png)

### Unlock

A key raises the unlock prompt; the password is checked through PAM. (`P-LK-01`)

![Unlock](walkthrough/unlock.png)

### Display scale

A 125% scale set through Mutter's DisplayConfig applies live. (`P-SY-03`)

![Display scale](walkthrough/scale.png)
