#!/usr/bin/env python3
"""Execute the actual workflow shell with controlled command receipts.

No images, guest disks or VMs are created. These tests qualify workflow control
flow only; six real acquisitions and measurement qualification remain CI work.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def measurement_script():
    source = (ROOT / '.github/workflows/performance-baseline.yml').read_text()
    marker = '      - name: Measure three instrumented GNOME then Roost pairs on this same host\n        run: |\n'
    if source.count(marker) != 1:
        raise ValueError('exact measurement step missing or duplicated')
    body = source.split(marker, 1)[1].split('\n      - ', 1)[0]
    lines = body.splitlines()
    if not lines or any(not line.startswith('          ') for line in lines if line.strip()):
        raise ValueError('unexpected workflow shell indentation')
    return '\n'.join(line[10:] for line in lines) + '\n'


COMMAND = r'''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
name=Path(sys.argv[0]).name
args=sys.argv[1:]
row={'command':name,'args':args}
if name=='roost-vm-perf':
    row['desktop']=args[args.index('--desktop')+1]
    row['out']=args[args.index('--out')+1]
    row['meta']=[args[i+1] for i,v in enumerate(args) if v=='--meta']
    repeat=next(v.split('=',1)[1] for v in row['meta'] if v.startswith('acquisition_repeat='))
    row['case']=repeat+':'+row['desktop']
with open(os.environ['RECEIPTS'],'a') as stream:
    stream.write(json.dumps(row)+'\n')
if name=='roost-vm-perf':
    target=Path(row['out']);target.mkdir(parents=True,exist_ok=True)
    (target/'controlled-command-receipt.json').write_text(json.dumps(row))
    if row['case']==os.environ.get('FAIL_CASE'):
        sys.exit(9)
'''


class Repeatability(unittest.TestCase):
    def execute(self, failure=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = root / 'commands'
            commands.mkdir()
            (root / 'scripts').mkdir()
            for name in ('sudo', 'truncate', 'rm'):
                path = commands / name
                path.write_text(COMMAND)
                path.chmod(0o755)
            path = root / 'scripts/roost-vm-perf'
            path.write_text(COMMAND)
            path.chmod(0o755)
            for name in ('base-digest', 'shared-image-id', 'gnome-version', 'baseline-packages.txt'):
                (root / name).write_text('controlled-original-' + name + '\n')
            receipts = root / 'receipts.jsonl'
            env = dict(os.environ, PATH=str(commands) + os.pathsep + os.environ['PATH'],
                       RECEIPTS=str(receipts), GITHUB_SHA='controlled-source', FAIL_CASE=failure or '')
            result = subprocess.run(['bash', '-c', measurement_script()], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=30)
            rows = [json.loads(line) for line in receipts.read_text().splitlines()]
            artifacts = sorted(str(path.relative_to(root)) for path in (root / 'perf-artifacts').rglob('controlled-command-receipt.json'))
            return result, rows, artifacts

    def test_actual_shell_retains_six_distinct_fresh_guest_measurements_and_order_metadata(self):
        result, rows, artifacts = self.execute()
        self.assertEqual(result.returncode, 0, result.stderr)
        measurements = [row for row in rows if row['command'] == 'roost-vm-perf']
        self.assertEqual([row['case'] for row in measurements],
                         ['1:gnome', '1:roost', '2:gnome', '2:roost', '3:gnome', '3:roost'])
        expected = ['perf-artifacts/gnome', 'perf-artifacts/roost',
                    'perf-artifacts/repeat-2/gnome', 'perf-artifacts/repeat-2/roost',
                    'perf-artifacts/repeat-3/gnome', 'perf-artifacts/repeat-3/roost']
        self.assertEqual([row['out'] for row in measurements], expected)
        self.assertEqual(artifacts, sorted(path + '/controlled-command-receipt.json' for path in expected))
        self.assertEqual(sum(row['command'] == 'truncate' for row in rows), 6)
        installs = [row for row in rows if row['command'] == 'sudo' and 'bootc' in row['args']]
        self.assertEqual(len(installs), 6)
        self.assertTrue(all('install' in row['args'] and '--wipe' in row['args'] for row in installs))
        for row in measurements:
            self.assertIn('acquisition_order=gnome-first-fixed', row['meta'])
            self.assertIn('reference_kind=instrumented-gnome51', row['meta'])
            self.assertIn('observer_revision=controlled-source', row['meta'])

    def test_first_and_later_rejections_preserve_nonzero_without_replay_or_selecting_later_passes(self):
        order = ['1:gnome', '1:roost', '2:gnome', '2:roost', '3:gnome', '3:roost']
        for failure in ('1:gnome', '2:gnome', '2:roost'):
            with self.subTest(failure=failure):
                result, rows, artifacts = self.execute(failure)
                self.assertEqual(result.returncode, 9)
                expected = order[:order.index(failure)+1]
                self.assertEqual([row['case'] for row in rows if row['command'] == 'roost-vm-perf'], expected)
                self.assertEqual(len(artifacts), len(expected))
                self.assertEqual(sum(row['command'] == 'truncate' for row in rows), len(expected))

    def test_failure_evidence_upload_remains_unconditional(self):
        source = (ROOT / '.github/workflows/performance-baseline.yml').read_text()
        suffix = source.split('      - name: Measure three instrumented GNOME then Roost pairs on this same host', 1)[1]
        self.assertIn('        if: always()\n', suffix)
        self.assertIn('          path: perf-artifacts\n', suffix)
        self.assertIn('- run: python3 scripts/lib/roost-perf-repeatability-tests.py', source)


if __name__ == '__main__':
    unittest.main()
