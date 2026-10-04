#!/usr/bin/env python3
"""Missing evidence or nonexecuted security cases must never qualify a run."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('security_artifacts', str(ROOT / 'scripts/roost-security-artifacts'))
spec = importlib.util.spec_from_loader(loader.name, loader)
gate = importlib.util.module_from_spec(spec)
loader.exec_module(gate)
REVISION = 'a' * 40


def fixture(root):
    for name in gate.REQUIRED:
        (root / name).write_text('fixture environment\n')
    (root / 'source-revision.txt').write_text(REVISION + '\n')
    (root / 'reproduce.txt').write_text('ROOST_REQUIRE_PAM_WRAPPER=1 cargo llvm-cov --workspace --locked\n')
    (root / 'coverage.json').write_text(json.dumps({'type': 'llvm.coverage.json.export', 'data': [{}]}))
    names = [name for required in gate.strategy.BOUNDARIES.values() for _, name in required.values()]
    (root / 'test.log').write_text('\n'.join(f'test {name} ... ok' for name in names) + '\n')


class Evidence(unittest.TestCase):
    def test_complete_passing_cases_and_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            self.assertEqual(len(gate.check(root, REVISION)), 10)

    def test_each_required_file_is_mandatory(self):
        for name in gate.REQUIRED:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                fixture(root)
                (root / name).unlink()
                with self.assertRaisesRegex(ValueError, 'missing or empty'):
                    gate.check(root, REVISION)

    def test_blank_environment_is_not_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            (root / 'packages.txt').write_text(' \n')
            with self.assertRaisesRegex(ValueError, 'blank required'):
                gate.check(root, REVISION)

    def test_wrong_source_and_optional_pam_do_not_qualify(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture(root)
            with self.assertRaisesRegex(ValueError, 'source does not match'):
                gate.check(root, 'b' * 40)
            (root / 'reproduce.txt').write_text('cargo llvm-cov --workspace')
            with self.assertRaisesRegex(ValueError, 'require the real PAM'):
                gate.check(root, REVISION)

    def test_failed_ignored_and_only_listed_cases_do_not_qualify(self):
        for result in ('FAILED', 'ignored', 'test'):
            with self.subTest(result=result), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                fixture(root)
                path = root / 'test.log'
                name = 'unapproved_release_is_a_protocol_error_without_clearing_the_owner'
                path.write_text(path.read_text().replace(f'test {name} ... ok', f'test {name} ... {result}'))
                with self.assertRaisesRegex(ValueError, 'no passing executed security case'):
                    gate.check(root, REVISION)


if __name__ == '__main__':
    unittest.main()
