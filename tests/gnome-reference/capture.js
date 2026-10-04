// GNOME 51 reference capture for Roost's pixel-parity work.
// Run as: gnome-shell --headless --virtual-monitor WxH --automation-script capture.js
// Writes OUT/<state>.png and OUT/<state>.json (visible styled actors).
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Mtk from 'gi://Mtk';
import Shell from 'gi://Shell';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Scripting from 'resource:///org/gnome/shell/ui/scripting.js';
import * as BoxPointer from 'resource:///org/gnome/shell/ui/boxpointer.js';

Gio._promisify(Shell.Screenshot.prototype, 'screenshot');
Gio._promisify(Shell.Screenshot.prototype, 'screenshot_stage_to_content');
Gio._promisify(Shell.Screenshot, 'composite_to_stream');

Gio._promisify(Gio.Subprocess.prototype, 'communicate_utf8_async', 'communicate_utf8_finish');

const OUT = GLib.getenv('GREF_OUT') ?? '/out';
export const METRICS = {};

function dump(actor, depth, out) {
    if (!actor.visible || depth > 40)
        return;
    const style = actor instanceof St.Widget ? actor.get_style_class_name() : null;
    const name = actor.get_name?.() ?? null;
    const type = actor.constructor.$gtype.name;
    if (style || name || /WindowPreview/.test(type)) {
        const [x, y] = actor.get_transformed_position();
        const [w, h] = actor.get_transformed_size();
        if (w > 0 && h > 0) {
            const entry = {
                class: style, name, type,
                rect: [Math.round(x), Math.round(y), Math.round(w), Math.round(h)],
            };
            if (actor instanceof St.Label || actor instanceof St.Button)
                entry.text = actor.text ?? actor.label ?? null;
            if (actor instanceof St.Widget) {
                const node = actor.get_theme_node();
                try {
                    entry.font = node.get_font().to_string();
                    entry.fg = node.get_foreground_color().to_string();
                    entry.bg = node.get_background_color().to_string();
                } catch {}
            }
            out.push(entry);
        }
    }
    for (const child of actor.get_children())
        dump(child, depth + 1, out);
}

// The lock screen blanks ordinary screenshots (privacy); GNOME's own
// screenshot UI composes the stage content instead.
async function shotStage(state) {
    await Scripting.sleep(700);
    const [content] = await new Shell.Screenshot().screenshot_stage_to_content();
    const texture = content.get_texture();
    const file = Gio.File.new_for_path(`${OUT}/${state}.png`);
    const stream = file.replace(null, false, Gio.FileCreateFlags.NONE, null);
    await Shell.Screenshot.composite_to_stream(texture, 0, 0,
        texture.get_width(), texture.get_height(), 1, null, 0, 0, 1, stream);
    stream.close(null);
    const actors = [];
    dump(global.stage, 0, actors);
    GLib.file_set_contents(`${OUT}/${state}.json`, JSON.stringify(actors, null, 1));
    windowState(state);
    print(`GREF captured ${state}`);
}

async function shotNow(state) {
    const file = Gio.File.new_for_path(`${OUT}/${state}.png`);
    const stream = file.replace(null, false, Gio.FileCreateFlags.NONE, null);
    await new Shell.Screenshot().screenshot(false, stream);
    stream.close(null);
    const actors = [];
    dump(global.stage, 0, actors);
    GLib.file_set_contents(`${OUT}/${state}.json`, JSON.stringify(actors, null, 1));
    windowState(state);
    print(`GREF captured ${state}`);
}

async function shot(state) {
    await Scripting.sleep(700);
    await Scripting.waitLeisure();
    const file = Gio.File.new_for_path(`${OUT}/${state}.png`);
    const stream = file.replace(null, false, Gio.FileCreateFlags.NONE, null);
    await new Shell.Screenshot().screenshot(false, stream);
    stream.close(null);
    const actors = [];
    dump(global.stage, 0, actors);
    GLib.file_set_contents(`${OUT}/${state}.json`, JSON.stringify(actors, null, 1));
    windowState(state);
    print(`GREF captured ${state}`);
}

function windowState(state) {
    const focused = global.display.focus_window;
    GLib.file_set_contents(`${OUT}/${state}.state.json`, JSON.stringify({
        active_workspace: global.workspace_manager.get_active_workspace_index(),
        focused: focused?.get_stable_sequence() ?? null,
        focused_title: focused?.get_title() ?? null,
        windows: global.get_window_actors().map(actor => {
            const win = actor.meta_window;
            const rect = win.get_frame_rect();
            return {id: win.get_stable_sequence(), title: win.get_title(),
                workspace: win.get_workspace().index(), maximized: win.get_maximize_flags(),
                rect: [rect.x, rect.y, rect.width, rect.height]};
        }),
    }, null, 1));
}

async function a11y(state) {
    // Await the subprocess without blocking GNOME's main loop: it must
    // answer the AT-SPI client's requests while the tree is being read.
    const child = Gio.Subprocess.new(['/usr/bin/python3', '/proof-lib/roost-a11y-dump.py',
        'gnome-shell', `${OUT}/a11y-${state}.json`, '20'],
        Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_PIPE);
    const result = await child.communicate_utf8_async(null, null);
    const [stdout, stderr] = result.slice(-2);
    if (!child.get_successful())
        throw new Error(`GNOME AT-SPI ${state}: ${stderr}`);
    print(stdout.trim());
}

let keyboard = null;
async function chord(keys) {
    keyboard ??= global.stage.context.get_backend().get_default_seat()
        .create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
    for (const key of keys) {
        keyboard.notify_keyval(GLib.get_monotonic_time(), key, Clutter.KeyState.PRESSED);
        await Scripting.sleep(80);
    }
    for (const key of [...keys].reverse()) {
        keyboard.notify_keyval(GLib.get_monotonic_time(), key, Clutter.KeyState.RELEASED);
        await Scripting.sleep(80);
    }
    await Scripting.sleep(700);
}

const NONE = BoxPointer.PopupAnimation.NONE;

export async function run() {
    await Scripting.disableHelperAutoExit();
    // GNOME opens the overview at login.
    await shot('00-startup-overview');
    Main.overview.hide();
    await Scripting.sleep(1500);
    await shot('01-desktop');
    await a11y('panel');

    Main.panel.statusArea.dateMenu.menu.open(NONE);
    await shot('02-calendar');
    Main.panel.statusArea.dateMenu.menu.close(NONE);

    const quickSettings = Main.panel.statusArea.quickSettings;
    quickSettings.menu.open(NONE);
    await shot('03-quick-settings');
    await a11y('quick-settings');
    // The Power Mode toggle's own menu, opened in place.
    const powerMode = quickSettings._powerProfiles.quickSettingsItems[0];
    powerMode.menu.open(false);
    await shot('03b-power-mode-menu');
    powerMode.menu.close(false);
    // The shutdown menu (status/system.js), opened in place too.
    const system = quickSettings._system.quickSettingsItems[0];
    system.menu.open(false);
    await shot('03c-power-menu');
    system.menu.close(false);
    quickSettings.menu.close(NONE);

    Main.overview.show();
    await Scripting.sleep(1500);
    await shot('04-overview-empty');
    await a11y('overview');
    // What a click on the dash's Show Apps button does.
    Main.overview.dash.showAppsButton.checked = true;
    await Scripting.sleep(1500);
    await shot('05-app-grid');
    // The System folder opened from the grid: GNOME's folder dialog.
    {
        const appDisplay = Main.overview._overview.controls._appDisplay;
        const folder = appDisplay._items.get('System');
        if (folder) {
            folder.open();
            await Scripting.sleep(1200);
            await shot('05b-app-folder');
            folder._dialog?.popdown();
            await Scripting.sleep(800);
        } else {
            print('GREF no System folder');
        }
    }
    Main.overview.dash.showAppsButton.checked = false;
    await Scripting.sleep(1500);
    // Typing in the overview: the search results (apps only, external
    // providers off on both sides).
    Main.overview.searchEntry.grab_key_focus();
    Main.overview.searchEntry.set_text('calc');
    await Scripting.sleep(1500);
    await shot('18-overview-search');
    Main.overview.searchEntry.set_text('');
    Main.overview.hide();
    await Scripting.sleep(1500);

    // The same libadwaita windows Roost's capture opens, so window
    // placement, stacking and decorations compare pixel for pixel.
    const testWindows = [];
    for (const [title, color] of [['Alpha', '#3584e4'], ['Beta', '#2ec27e'], ['Gamma', '#e66100']]) {
        testWindows.push(Gio.Subprocess.new(
            ['/usr/bin/python3', '/proof-lib/roost-test-window.py', title, color],
            Gio.SubprocessFlags.NONE));
        // One at a time, as Roost's capture maps them in order.
        for (let t = 0; t < 100 && global.get_window_actors().length < testWindows.length; t++)
            await Scripting.sleep(100);
    }
    await Scripting.sleep(3000);
    await shot('06-windows');
    // Alt+Tab, as GNOME draws it (shown without a held modifier, it
    // stays up for NO_MODS_TIMEOUT).
    const AltTab = await import('resource:///org/gnome/shell/ui/altTab.js');
    const switcher = new AltTab.AppSwitcherPopup();
    switcher.show(false, 'switch-applications', 0);
    await Scripting.sleep(400);
    await shotNow('06b-switcher');
    // Releasing Alt: switch to the selected app (deterministic, unlike
    // the no-modifier timeout racing the popup's teardown).
    if (switcher.get_parent())
        switcher._finish(global.display.get_current_time_roundtrip());
    else
        switcher.destroy();
    await Scripting.sleep(500);
    const previousFocus = global.display.focus_window;
    Main.overview.show();
    await Scripting.sleep(1500);
    await shot('07-overview-windows');
    await a11y('overview');
    // The first preview as hovered: GNOME's own hover path.
    const previews = [];
    const find = a => {
        if (a.constructor.$gtype.name.includes('WindowPreview'))
            previews.push(a);
        a.get_children().forEach(find);
    };
    find(global.stage);
    previews.sort((a, b) => a.get_transformed_position()[0] - b.get_transformed_position()[0] ||
        a.get_transformed_position()[1] - b.get_transformed_position()[1]);
    previews[0]?.showOverlay(false);
    await shot('07b-overview-hover');
    previews[0]?.hideOverlay(false);
    await chord([Clutter.KEY_Escape]);
    if (global.display.focus_window !== previousFocus)
        throw new Error('Escape did not restore the previous focus');
    await shot('20-overview-dismiss-focus');

    const layoutWindow = global.display.focus_window;
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Up]);
    if (layoutWindow.get_maximize_flags() !== Meta.MaximizeFlags.BOTH)
        throw new Error('Super+Up did not maximize');
    await shot('21-maximized');
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Down]);
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Left]);
    await shot('22-tiled-left');
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Down]);
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Right]);
    await shot('22b-tiled-right');
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Down]);

    // The same D-Bus notification Roost's capture sends.
    Gio.DBus.session.call('org.freedesktop.Notifications',
        '/org/freedesktop/Notifications', 'org.freedesktop.Notifications', 'Notify',
        new GLib.Variant('(susssasa{sv}i)', ['System', 0, 'cog-wheel-symbolic',
            'Roost reference', 'A notification banner, as GNOME 51 draws it',
            [], {}, 5000]),
        null, Gio.DBusCallFlags.NONE, -1, null, null);
    await Scripting.sleep(1000);
    await shot('08-notification');
    // A scripted session has no input, so the tray would keep the banner
    // for an away user forever. The next step is a click for a real
    // user: tell the tray they are back, as its idle watch would.
    Main.messageTray._onIdleMonitorBecameActive?.();
    await Scripting.sleep(6000);

    Main.panel.statusArea.dateMenu.menu.open(NONE);
    await shot('09-calendar-with-notification');
    Main.panel.statusArea.dateMenu.menu.close(NONE);

    // GNOME's end-session dialog, as gnome-session opens it (Log Out,
    // 60 s, no inhibitors), over its own D-Bus object.
    Gio.DBus.session.call(Gio.DBus.session.unique_name,
        '/org/gnome/SessionManager/EndSessionDialog',
        'org.gnome.SessionManager.EndSessionDialog', 'Open',
        new GLib.Variant('(uuuao)', [0, 0, 60, []]), null,
        Gio.DBusCallFlags.NONE, -1, null, null);
    await Scripting.sleep(1500);
    await shot('10-end-session');
    Gio.DBus.session.call(Gio.DBus.session.unique_name,
        '/org/gnome/SessionManager/EndSessionDialog',
        'org.gnome.SessionManager.EndSessionDialog', 'Close',
        null, null, Gio.DBusCallFlags.NONE, -1, null, null);
    await Scripting.sleep(1000);

    // The OSD, as gnome-settings-daemon's ShowOSD calls draw it: a volume
    // level, a labelled one (a keyboard layout), and volume past 100%.
    const osd = (icon, label, level, maxLevel) => Main.osdWindowManager.showAll(
        Gio.Icon.new_for_string(icon), label, level, maxLevel);
    osd('audio-volume-medium-symbolic', null, 0.5, 1);
    await Scripting.sleep(500);
    await shot('12-osd-volume');
    osd('input-keyboard-symbolic', 'English (US)', null, null);
    await Scripting.sleep(500);
    await shot('12b-osd-label');
    osd('audio-volume-overamplified-symbolic', null, 1.25, 1.5);
    await Scripting.sleep(500);
    await shot('12c-osd-overdrive');
    await Scripting.sleep(2000);

    // The window menu, as a right click on the focused window's header
    // bar at (500, 280) opens it (Roost's capture right-clicks there).
    const focused = global.display.focus_window;
    if (focused) {
        const frame = focused.get_frame_rect();
        Main.wm._windowMenuManager.showWindowMenuForWindow(focused,
            Meta.WindowMenuType.WM, {x: frame.x + 60, y: frame.y + 20, width: 0, height: 0});
        await Scripting.sleep(600);
        await shot('13-window-menu');
        Main.wm._windowMenuManager._manager.activeMenu?.close();
        await Scripting.sleep(500);
    }

    // The workspace switcher popup, as Super+Page_Down shows it: to the
    // empty second workspace and back.
    const WSP = await import('resource:///org/gnome/shell/ui/workspaceSwitcherPopup.js');
    const wm = global.workspace_manager;
    Main.wm.actionMoveWorkspace(wm.get_workspace_by_index(1));
    const popup = new WSP.WorkspaceSwitcherPopup();
    popup.display(1);
    await Scripting.sleep(350);
    await shotNow('15-workspace-popup');
    Main.wm.actionMoveWorkspace(wm.get_workspace_by_index(0));
    await Scripting.sleep(1200);
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Page_Down]);
    if (wm.get_active_workspace_index() !== 1)
        throw new Error('Super+Page_Down did not switch workspace');
    await shot('23-workspace-switched');
    await chord([Clutter.KEY_Super_L, Clutter.KEY_Page_Up]);
    if (wm.get_active_workspace_index() !== 0)
        throw new Error('Super+Page_Up did not restore workspace');

    // The screenshot UI (Print): the frozen screen, dimmed outside the
    // selection, with its panel.
    await Main.screenshotUI.open();
    await Scripting.sleep(1200);
    await shotStage('14-screenshot-ui');
    Main.screenshotUI.close(true);
    await Scripting.sleep(800);

    // Three workspaces (the focused window moved to the second, the view
    // staying on the first): the overview's workspace thumbnails.
    {
        const win = global.display.focus_window;
        if (win) {
            win.change_workspace_by_index(1, false);
            await Scripting.sleep(500);
            Main.overview.show();
            await Scripting.sleep(1500);
            await shot('16-overview-workspaces');
            // Use GNOME's actual drag-over target, then capture the
            // insertion slot and shifted thumbnails before dropping.
            let thumbs = null;
            let preview = null;
            const findDragActors = actor => {
                if (actor instanceof St.Widget && actor.has_style_class_name('workspace-thumbnails'))
                    thumbs = actor;
                if (!preview && actor.constructor.$gtype.name.includes('WindowPreview'))
                    preview = actor;
                actor.get_children().forEach(findDragActors);
            };
            findDragActors(global.stage);
            if (!thumbs || !preview)
                throw new Error('workspace insertion capture has no thumbnail box or preview');
            const second = thumbs._thumbnails[1];
            thumbs.handleDragOver(preview, preview, second.x - 3, second.y + second.height / 2, global.get_current_time());
            await shot('16b-workspace-insertion-placeholder');
            thumbs._dropPlaceholderPos = -1;
            thumbs.queue_relayout();

            Main.overview.hide();
            await Scripting.sleep(1000);
        }
    }

    // Edge tiling: the tile preview over the left half while Gamma is
    // dragged to the left edge. Headless input is not reliable here,
    // so Gamma moves by the drag's offset (header 600,328 to 1,400)
    // and GNOME's own handler opens the preview.
    {
        const win = global.display.focus_window;
        if (win) {
            const r = win.get_frame_rect();
            win.move_frame(true, r.x - 599, r.y + 72);
            await Scripting.sleep(500);
            const wa = Main.layoutManager.getWorkAreaForMonitor(0);
            const tile = new Mtk.Rectangle({
                x: wa.x, y: wa.y, width: Math.floor(wa.width / 2), height: wa.height,
            });
            Main.wm._showTilePreview(global.window_manager, win, tile, 0);
            await Scripting.sleep(1000);
            await shot('17-tile-preview');
            Main.wm._hideTilePreview();
            win.move_frame(true, r.x, r.y);
            await Scripting.sleep(800);
        }
    }

    // An app's popover (an xdg popup): a fourth test window opens its
    // header-bar menu by itself, as Roost's capture does.
    {
        const launcher = new Gio.SubprocessLauncher({flags: Gio.SubprocessFlags.NONE});
        launcher.setenv('ROOST_TEST_POPOVER', '1', true);
        const before = global.get_window_actors().length;
        const delta = launcher.spawnv(['/usr/bin/python3', '/proof-lib/roost-test-window.py', 'Delta', '#c01c28']);
        for (let t = 0; t < 100 && global.get_window_actors().length <= before; t++)
            await Scripting.sleep(100);
        await Scripting.sleep(3000);
        await shot('19-app-popover');
        delta.force_exit();
        await Scripting.sleep(1000);
    }

    // The lock screen: the curtain with the clock, then the unlock prompt.
    if (Main.screenShield) {
        Main.screenShield.lock(false);
        await Scripting.sleep(3000);
        // Without gnome-settings-daemon the shield stays faded to black
        // (its blanking lightbox); lift it to show the lock screen.
        Main.screenShield._shortLightbox?.lightOff(0);
        Main.screenShield._longLightbox?.lightOff(0);
        await Scripting.sleep(500);
        await shotStage('11-lock-screen');
        Main.screenShield.showDialog();
        // The stubbed display manager fails every verification, which
        // would send the dialog back to the curtain; keep the prompt.
        const dialog = Main.screenShield._dialog;
        if (dialog)
            dialog._fail = () => {};
        dialog?._showPrompt?.();
        await Scripting.sleep(1000);
        // Without GDM's PAM conversation or AccountsService the prompt
        // shows an error and no name. Put it in the state a real
        // session shows: the user's name and the password question,
        // as GDM's 'Password:' prompt sets it.
        await Scripting.sleep(1000);
        if (dialog && dialog._activePage !== dialog._promptBox)
            dialog._showPrompt();
        await Scripting.sleep(500);
        const prompt = Main.screenShield._dialog?._authPrompt;
        if (prompt) {
            prompt.setMessage(null);
            prompt.setQuestion('Password');
            // AccountsService's real name, as UserWidgetLabel shows it.
            const name = GLib.getenv('GREF_USER_NAME') || 'User';
            const label = prompt._userWell.get_child()?._label;
            for (const l of [label?._realNameLabel, label?._userNameLabel])
                if (l)
                    l.text = name;
            if (label && !label._realNameLabel)
                label.text = name;
            if (label) {
                label.opacity = 255;
                label.queue_relayout();
            }
        }
        await Scripting.sleep(500);
        await shotStage('11b-unlock-prompt');
    } else {
        print('GREF no screen shield');
    }

    for (const proc of testWindows)
        proc.force_exit();
    print('GREF done');
}
