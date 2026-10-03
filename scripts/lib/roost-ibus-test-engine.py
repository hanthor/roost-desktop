#!/usr/bin/python3
"""A tiny IBus engine for the IME proof.

Usage: roost-ibus-test-engine.py

Registers engine "roost-test" with the running ibus-daemon and makes it
the global engine. Letters build a preedit (pinyin); Space commits its
hanzi from a small table ("nihao" -> 你好), else the letters; BackSpace
edits the preedit. Prints "roost-ibus-test-engine: ready" once set.
"""
import gi

gi.require_version("IBus", "1.0")
from gi.repository import GLib, GObject, IBus  # noqa: E402

TABLE = {"nihao": "你好", "ni": "你", "hao": "好"}
BLOCKING = (
    IBus.ModifierType.CONTROL_MASK
    | IBus.ModifierType.MOD1_MASK
    | IBus.ModifierType.SUPER_MASK
    | IBus.ModifierType.MOD4_MASK
)


class RoostTestEngine(IBus.Engine):
    __gtype_name__ = "RoostTestEngine"

    def __init__(self):
        super().__init__()
        self.buffer = ""

    def show(self):
        text = IBus.Text.new_from_string(self.buffer)
        self.update_preedit_text(text, len(self.buffer), bool(self.buffer))

    def do_process_key_event(self, keyval, keycode, state):
        if state & IBus.ModifierType.RELEASE_MASK or state & BLOCKING:
            return False
        ch = IBus.keyval_to_unicode(keyval)
        if len(ch) == 1 and "a" <= ch <= "z":
            self.buffer += ch
            self.show()
            return True
        if keyval == IBus.KEY_space and self.buffer:
            self.commit_text(IBus.Text.new_from_string(TABLE.get(self.buffer, self.buffer)))
            self.buffer = ""
            self.show()
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
        raise SystemExit("roost-ibus-test-engine: no ibus-daemon")
    factory = IBus.Factory.new(bus.get_connection())
    factory.add_engine("roost-test", GObject.type_from_name("RoostTestEngine"))
    component = IBus.Component(
        name="org.roost.TestEngine",
        description="Roost IME proof engine",
        version="1.0",
        license="MIT",
        author="Roost",
        homepage="https://github.com/hanthor/roost-desktop",
        command_line="",
        textdomain="",
    )
    component.add_engine(
        IBus.EngineDesc(
            name="roost-test",
            longname="Roost Test",
            description="Pinyin to hanzi for the IME proof",
            language="zh",
            license="MIT",
            author="Roost",
            icon="",
            layout="us",
        )
    )
    bus.register_component(component)

    # Async: the daemon creates the engine through this process's
    # factory while the call is pending, so a sync call would deadlock.
    def set_done(bus, result):
        if bus.set_global_engine_async_finish(result):
            print("roost-ibus-test-engine: ready", flush=True)
        else:
            raise SystemExit("roost-ibus-test-engine: could not set the engine")

    bus.set_global_engine_async("roost-test", -1, None, set_done)
    GLib.MainLoop().run()


main()
