#!/usr/bin/env python3
"""Negative fixtures ensure incomplete or forged coverage cannot pass."""
import copy
import importlib.machinery
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('coverage_report', str(ROOT / 'scripts/roost-coverage-report'))
spec = importlib.util.spec_from_loader(loader.name, loader)
report = importlib.util.module_from_spec(spec)
loader.exec_module(report)


def fixture():
    paths = set(report.SECURITY)
    for manifest in (ROOT / 'crates').glob('*/Cargo.toml'):
        paths.add(next((manifest.parent / 'src').glob('*.rs')).relative_to(ROOT).as_posix())
    files = [{'filename': str(ROOT / path),
              'summary': {'lines': {'count': 10, 'covered': 6},
                          'regions': {'count': 20, 'covered': 8}}} for path in sorted(paths)]
    return {'type': 'llvm.coverage.json.export', 'data': [{'files': files}]}


class CoverageReport(unittest.TestCase):
    def test_aggregates_source_counts_and_excludes_foreign_files(self):
        document = fixture()
        document['data'][0]['files'].append({'filename': '/foreign/crates/compositor/src/lock.rs'})
        crates, files = report.measure(document, ROOT)
        self.assertEqual(len(crates), len(list((ROOT / 'crates').glob('*/Cargo.toml'))))
        self.assertEqual(files[report.SECURITY[0]]['lines'], (6, 10))

    def test_rejects_missing_crate_even_if_security_files_exist(self):
        document = fixture()
        document['data'][0]['files'] = [entry for entry in document['data'][0]['files']
                                       if '/wallpaper/src/' not in entry['filename']]
        with self.assertRaisesRegex(ValueError, 'missing measured source coverage'):
            report.measure(document, ROOT)

    def test_rejects_missing_security_module(self):
        document = fixture()
        document['data'][0]['files'] = [entry for entry in document['data'][0]['files']
                                       if not entry['filename'].endswith('/session_lock.rs')]
        with self.assertRaisesRegex(ValueError, 'missing security module'):
            report.measure(document, ROOT)

    def test_rejects_double_counted_file(self):
        document = fixture()
        document['data'][0]['files'].append(copy.deepcopy(document['data'][0]['files'][0]))
        with self.assertRaisesRegex(ValueError, 'duplicate coverage file'):
            report.measure(document, ROOT)

    def test_rejects_impossible_and_boolean_counts(self):
        for covered in (11, -1, True):
            document = fixture()
            document['data'][0]['files'][0]['summary']['lines']['covered'] = covered
            with self.assertRaisesRegex(ValueError, 'invalid lines counts'):
                report.measure(document, ROOT)

    def test_floors_reject_regression_and_zero_instrumentation(self):
        _, files = report.measure(fixture(), ROOT)
        policy = {'modules': {path: {'lines': 60, 'regions': 40} for path in report.SECURITY}}
        self.assertEqual(report.enforce(files, policy), [])
        files[report.SECURITY[0]]['lines'] = (5, 10)
        self.assertEqual(len(report.enforce(files, policy)), 1)
        files[report.SECURITY[0]]['lines'] = (0, 0)
        self.assertEqual(len(report.enforce(files, policy)), 1)

    def test_floors_cannot_omit_modules_or_disable_a_counter(self):
        _, files = report.measure(fixture(), ROOT)
        policy = {'modules': {path: {'lines': 60, 'regions': 40} for path in report.SECURITY}}
        incomplete = copy.deepcopy(policy)
        del incomplete['modules'][report.SECURITY[0]]
        with self.assertRaisesRegex(ValueError, 'exactly the required'):
            report.enforce(files, incomplete)
        for floor in (0, True, 101):
            invalid = copy.deepcopy(policy)
            invalid['modules'][report.SECURITY[0]]['lines'] = floor
            with self.assertRaisesRegex(ValueError, 'invalid lines floor'):
                report.enforce(files, invalid)

    def test_rejects_empty_or_wrong_report_type(self):
        for document in ({'type': 'test-inventory'}, {'type': 'llvm.coverage.json.export', 'data': []}):
            with self.assertRaises(ValueError):
                report.measure(document, ROOT)


if __name__ == '__main__':
    unittest.main()
