#!/usr/bin/python3
"""Exercise the proof cleanup gate without starting graphical processes."""
import ast
from pathlib import Path
import unittest

source = Path(__file__).resolve().parents[1] / 'tuna-window-lifecycle-proof'
module = ast.parse(source.read_text())
gate = next(node for node in module.body if isinstance(node, ast.FunctionDef) and node.name == 'require_owned_cleanup')
namespace = {}
exec(compile(ast.Module(body=[gate], type_ignores=[]), str(source), 'exec'), namespace)

class CleanupGate(unittest.TestCase):
    def test_cleanup_failure_is_fatal(self):
        for runtime, processes in ((False, True), (True, False), (False, False)):
            with self.assertRaises(AssertionError):
                namespace['require_owned_cleanup']({'runtime_removed':runtime, 'accessibility_processes_stopped':processes})

    def test_success_requires_both_owned_resources_removed(self):
        namespace['require_owned_cleanup']({'runtime_removed':True, 'accessibility_processes_stopped':True})

    def test_pass_artifact_is_written_after_cleanup_gate(self):
        text = source.read_text()
        self.assertGreater(text.index('(art / "assertions.txt").write_text'), text.index('require_owned_cleanup(manifest['))

if __name__ == '__main__':
    unittest.main()
