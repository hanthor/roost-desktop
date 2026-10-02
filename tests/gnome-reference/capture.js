// GNOME 51 reference capture for Roost's pixel-parity work.
// Run as: gnome-shell --headless --virtual-monitor WxH --automation-script capture.js
// Writes OUT/<state>.png and OUT/<state>.json (visible styled actors).
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
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
    Main.overview.dash.showAppsButton.checked = false;
    Main.overview.hide();
    await Scripting.sleep(1500);

    for (let i = 0; i < 3; i++)
        await Scripting.createTestWindow({width: 640, height: 420});
    await Scripting.waitTestWindows();
    await shot('06-windows');
    // Alt+Tab, as GNOME draws it (shown without a held modifier, it
    // stays up for NO_MODS_TIMEOUT).
    const AltTab = await import('resource:///org/gnome/shell/ui/altTab.js');
    const switcher = new AltTab.AppSwitcherPopup();
    switcher.show(false, 'switch-applications', 0);
    await Scripting.sleep(400);
    await shotNow('06b-switcher');
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

    Main.notify('Roost reference', 'A notification banner, as GNOME 51 draws it');
    await shot('08-notification');
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
        Main.screenShield._dialog?._showPrompt?.();
        await Scripting.sleep(1000);
        // Without GDM's PAM conversation or AccountsService the prompt
        // shows an error and no name. Put it in the state a real
        // session shows: the user's name and the password question,
        // as GDM's 'Password:' prompt sets it.
        await Scripting.sleep(1000);
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

    await Scripting.destroyTestWindows();
    print('GREF done');
}
