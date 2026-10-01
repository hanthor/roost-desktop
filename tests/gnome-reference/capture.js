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

const OUT = GLib.getenv('GREF_OUT') ?? '/out';
export const METRICS = {};

function dump(actor, depth, out) {
    if (!actor.visible || depth > 40)
        return;
    const style = actor instanceof St.Widget ? actor.get_style_class_name() : null;
    const name = actor.get_name?.() ?? null;
    if (style || name) {
        const [x, y] = actor.get_transformed_position();
        const [w, h] = actor.get_transformed_size();
        if (w > 0 && h > 0) {
            const entry = {
                class: style, name, type: actor.constructor.$gtype.name,
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
    quickSettings.menu.close(NONE);

    Main.overview.show();
    await Scripting.sleep(1500);
    await shot('04-overview-empty');
    Main.overview.showApps();
    await Scripting.sleep(1500);
    await shot('05-app-grid');
    Main.overview.hide();
    await Scripting.sleep(1500);

    for (let i = 0; i < 3; i++)
        await Scripting.createTestWindow({width: 640, height: 420});
    await Scripting.waitTestWindows();
    await shot('06-windows');
    Main.overview.show();
    await Scripting.sleep(1500);
    await shot('07-overview-windows');
    Main.overview.hide();
    await Scripting.sleep(1500);

    Main.notify('Roost reference', 'A notification banner, as GNOME 51 draws it');
    await shot('08-notification');
    await Scripting.sleep(6000);

    Main.panel.statusArea.dateMenu.menu.open(NONE);
    await shot('09-calendar-with-notification');
    Main.panel.statusArea.dateMenu.menu.close(NONE);

    await Scripting.destroyTestWindows();
    print('GREF done');
}
