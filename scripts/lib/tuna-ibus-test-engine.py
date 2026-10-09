#!/usr/bin/python3
"""A tiny IBus engine for the IME proof.

Usage: tuna-ibus-test-engine.py

Registers engine "tuna-test" with the running ibus-daemon and makes it
the global engine. Letters build a preedit (pinyin); Space commits its
hanzi from a small table ("nihao" -> 你好), else the letters; BackSpace
edits the preedit. F2 reads the surrounding text the client sent,
deletes the character before the cursor and commits "|TEXT|CURSOR" (so
"你好" with the cursor after it becomes "你|你好|2"). Prints
"tuna-ibus-test-engine: ready" once set, and each surrounding text and
cursor location IBus delivers ("surrounding TEXT CURSOR ANCHOR",
"cursor X Y W H").

Pinyin with several hanzi ("ni" -> 你, 尼, 泥) shows IBus's lookup table,
which the panel (Tuna Desktop's GTK shell, as GNOME Shell) draws; a click on
a candidate commits it ("candidate clicked INDEX BUTTON STATE" is
printed), as do 1-9 and Space (the highlighted one).
"""
import gi

gi.require_version("IBus", "1.0")
from gi.repository import GLib, GObject, IBus  # noqa: E402

TABLE = {"nihao": "你好", "ni": "你", "hao": "好"}
CANDIDATES = {"ni": ["你", "尼", "泥"]}
BLOCKING = (
    IBus.ModifierType.CONTROL_MASK
    | IBus.ModifierType.MOD1_MASK
    | IBus.ModifierType.SUPER_MASK
    | IBus.ModifierType.MOD4_MASK
)


class TunaTestEngine(IBus.Engine):
    __gtype_name__ = "TunaTestEngine"

    def __init__(self):
        super().__init__()
        self.buffer = ""
        self.surrounding = None
        self.table = IBus.LookupTable.new(5, 0, True, False)
        self.choices = []

    def do_set_surrounding_text(self, text, cursor_pos, anchor_pos):
        self.surrounding = (text.get_text(), cursor_pos)
        print(
            f"tuna-ibus-test-engine: surrounding {text.get_text()} {cursor_pos} {anchor_pos}",
            flush=True,
        )
        IBus.Engine.do_set_surrounding_text(self, text, cursor_pos, anchor_pos)

    def do_set_cursor_location(self, x, y, w, h):
        print(f"tuna-ibus-test-engine: cursor {x} {y} {w} {h}", flush=True)

    def show(self):
        text = IBus.Text.new_from_string(self.buffer)
        self.update_preedit_text(text, len(self.buffer), bool(self.buffer))
        self.choices = CANDIDATES.get(self.buffer, [])
        self.table.clear()
        for choice in self.choices:
            self.table.append_candidate(IBus.Text.new_from_string(choice))
        if self.choices:
            self.update_lookup_table(self.table, True)
        else:
            self.hide_lookup_table()

    def choose(self, index):
        """Commit candidate `index` of the current page."""
        page = self.table.get_cursor_pos() // self.table.get_page_size()
        index += page * self.table.get_page_size()
        if index >= len(self.choices):
            return False
        self.commit_text(IBus.Text.new_from_string(self.choices[index]))
        self.buffer = ""
        self.show()
        return True

    def do_candidate_clicked(self, index, button, state):
        print(f"tuna-ibus-test-engine: candidate clicked {index} {button} {state}", flush=True)
        self.choose(index)

    def do_cursor_up(self):
        if self.table.cursor_up():
            self.update_lookup_table(self.table, True)

    def do_cursor_down(self):
        if self.table.cursor_down():
            self.update_lookup_table(self.table, True)

    def do_page_up(self):
        if self.table.page_up():
            self.update_lookup_table(self.table, True)

    def do_page_down(self):
        if self.table.page_down():
            self.update_lookup_table(self.table, True)

    def do_process_key_event(self, keyval, keycode, state):
        if state & IBus.ModifierType.RELEASE_MASK or state & BLOCKING:
            return False
        ch = IBus.keyval_to_unicode(keyval)
        if len(ch) == 1 and "a" <= ch <= "z":
            self.buffer += ch
            self.show()
            return True
        if self.choices and "1" <= ch <= "9" and len(ch) == 1:
            return self.choose(int(ch) - 1)
        if keyval == IBus.KEY_space and self.buffer and self.buffer not in TABLE and self.choices:
            return self.choose(self.table.get_cursor_in_page())
        if keyval == IBus.KEY_space and self.buffer:
            self.commit_text(IBus.Text.new_from_string(TABLE.get(self.buffer, self.buffer)))
            self.buffer = ""
            self.show()
            return True
        if keyval == IBus.KEY_F2 and self.surrounding and not self.buffer:
            text, cursor = self.surrounding
            self.delete_surrounding_text(-1, 1)
            self.commit_text(IBus.Text.new_from_string(f"|{text}|{cursor}"))
            return True
        if keyval == IBus.KEY_BackSpace and self.buffer:
            self.buffer = self.buffer[:-1]
            self.show()
            return True
        return False

    def do_focus_out(self):
        self.buffer = ""
        self.show()


def main():
    IBus.init()
    bus = IBus.Bus()
    if not bus.is_connected():
        raise SystemExit("tuna-ibus-test-engine: no ibus-daemon")
    factory = IBus.Factory.new(bus.get_connection())
    factory.add_engine("tuna-test", GObject.type_from_name("TunaTestEngine"))
    component = IBus.Component(
        name="org.tuna.TestEngine",
        description="Tuna Desktop IME proof engine",
        version="1.0",
        license="MIT",
        author="Tuna Desktop",
        homepage="https://github.com/tuna-os/tuna-desktop",
        command_line="",
        textdomain="",
    )
    component.add_engine(
        IBus.EngineDesc(
            name="tuna-test",
            longname="Tuna Desktop Test",
            description="Pinyin to hanzi for the IME proof",
            language="zh",
            license="MIT",
            author="Tuna Desktop",
            icon="",
            layout="us",
        )
    )
    bus.register_component(component)

    # Async: the daemon creates the engine through this process's
    # factory while the call is pending, so a sync call would deadlock.
    def set_done(bus, result):
        if bus.set_global_engine_async_finish(result):
            print("tuna-ibus-test-engine: ready", flush=True)
        else:
            raise SystemExit("tuna-ibus-test-engine: could not set the engine")

    bus.set_global_engine_async("tuna-test", -1, None, set_done)
    GLib.MainLoop().run()


main()
