#!/usr/bin/env python3
"""Profiler patches must retain exact makepkg integrity after source edits."""
import hashlib
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2] / "packaging/marlin/perf/mutter-profiler"

class ProfilerPatchIntegrity(unittest.TestCase):
    def test_every_local_patch_has_its_current_blake2b_digest(self):
        recipe = (ROOT / "PKGBUILD").read_text()
        patches = re.findall(r"^  ([\w-]+\.patch)$", recipe, re.M)
        self.assertEqual(len(patches), 4)
        sums = re.search(r"b2sums=\((.*?)\)", recipe, re.S).group(1)
        hashes = re.findall(r"'([0-9a-f]{128})'", sums)
        # The first two sums pin upstream Mutter/gvdb, then local patches.
        self.assertEqual(len(hashes), len(patches) + 2)
        for patch, expected in zip(patches, hashes[2:]):
            with self.subTest(patch=patch):
                payload = (ROOT / patch).read_bytes()
                self.assertEqual(hashlib.blake2b(payload).hexdigest(), expected,
                                 "refresh the pinned digest when this patch changes")
                self.assertNotEqual(hashlib.blake2b(payload + b"\n").hexdigest(), expected)

    def test_patch_application_keeps_exact_context(self):
        recipe = (ROOT / "PKGBUILD").read_text()
        patches = re.findall(r"^  ([\w-]+\.patch)$", recipe, re.M)
        for patch in patches:
            self.assertIn(f'patch --batch --fuzz=0 -p1 < "$srcdir/{patch}"', recipe)

if __name__ == "__main__":
    unittest.main()
