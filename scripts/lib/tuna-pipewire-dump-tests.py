"""Withdrawal oracles require a complete snapshot despite removal races."""
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("dump", Path(__file__).with_name("tuna-pipewire-dump.py"))
dump = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dump)
CORE = {"id": 0, "type": "PipeWire:Interface:Core"}
NODE = {"id": 43, "type": "PipeWire:Interface:Node"}

class DumpTests(unittest.TestCase):
    def test_real_removal_before_complete_snapshot(self):
        raw = (json.dumps([{"id": 43, "info": None}]) + "\n" + json.dumps([CORE])).encode()
        self.assertEqual(dump.dump_objects(raw), [CORE])

    def test_node_seen_in_any_document_still_blocks_withdrawal(self):
        raw = (json.dumps([CORE, NODE]) + "\n" + json.dumps([{"id": 43, "info": None}]) + "\n" + json.dumps([CORE])).encode()
        self.assertIn(NODE, dump.dump_objects(raw))

    def test_no_core_empty_tombstone_only_and_broken_output_fail_closed(self):
        for raw in [b"", b"[]", b'[{"id":43,"info":null}]', b'[{"id":0,"type":"PipeWire:Interface:Core"}] [] garbage', b'[{"id":0,"type":"PipeWire:Interface:Core"}] [', b'{}', b'[{"id":43,"info":true}]']:
            with self.subTest(raw=raw), self.assertRaises((ValueError, json.JSONDecodeError)):
                dump.dump_objects(raw)

    def test_byte_and_document_budgets_fail_closed(self):
        for raw in [b" " * (8 * 1024 * 1024 + 1), (json.dumps([CORE]) + "\n" + "[]\n" * 64).encode()]:
            with self.assertRaises(ValueError):
                dump.dump_objects(raw)

if __name__ == "__main__":
    unittest.main()
