// GNOME 51 reference capture for Roost's pixel-parity work.
// Run as: gnome-shell --headless --virtual-monitor WxH --automation-script capture.js
// Writes OUT/<state>.png and OUT/<state>.json (visible styled actors).
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Scripting from 'resource:///org/gnome/shell/ui/scripting.js';
import * as BoxPointer from 'resource:///org/gnome/shell/ui/boxpointer.js';

Gio._promisify(Shell.Screenshot.prototype, 'screenshot');
Gio._promisify(Shell.Screenshot.prototype, 'screenshot_stage_to_content');
Gio._promisify(Shell.Screenshot, 'composite_to_stream');

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
    print(`GREF captured ${state}`);
}

const NONE = BoxPointer.PopupAnimation.NONE;

export async function run() {
    await Scripting.disableHelperAutoExit();
    // GNOME opens the overview at login.
    await shot('00-startup-overview');
    Main.overview.hide();
    await Scripting.sleep(1500);
    await shot('01-desktop');

    Main.panel.statusArea.dateMenu.menu.open(NONE);
    await shot('02-calendar');
    Main.panel.statusArea.dateMenu.menu.close(NONE);

    const quickSettings = Main.panel.statusArea.quickSettings;
    quickSettings.menu.open(NONE);
    await shot('03-quick-settings');
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
    Main.overview.hide();
    await Scripting.sleep(1500);

    // The same libadwaita windows Roost's capture opens, so window
    // placement, stacking and decorations compare pixel for pixel.
    const testWindows = [];
    for (const [title, color] of [['Alpha', '#3584e4'], ['Beta', '#2ec27e'], ['Gamma', '#e66100']]) {
        testWindows.push(Gio.Subprocess.new(
            ['/usr/bin/python3', '/lib/roost-test-window.py', title, color],
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
    Main.overview.show();
    await Scripting.sleep(1500);
    await shot('07-overview-windows');
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
    Main.overview.hide();
    await Scripting.sleep(1500);

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
        Main.wm._windowMenuManager.showWindowMenuForWindow(focused,
            Meta.WindowMenuType.WM, {x: 500, y: 280, width: 0, height: 0});
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

    // The screenshot UI (Print): the frozen screen, dimmed outside the
    // selection, with its panel.
    await Main.screenshotUI.open();
    await Scripting.sleep(1200);
    await shotStage('14-screenshot-ui');
    Main.screenshotUI.close(true);
    await Scripting.sleep(800);

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
