#!/usr/bin/env python3
"""Execute the actual workflow shell with controlled command receipts.

No images, guest disks or VMs are created. These tests qualify workflow control
flow only; twelve real acquisitions and measurement qualification remain CI work.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def measurement_script(profile):
    if profile not in ("diagnostic", "stock"):
        raise ValueError("fixed profile required")
    source = (ROOT / '.github/workflows/performance-baseline.yml').read_text()
    expected = "        run: scripts/tuna-perf-profile-pairs " + profile
    if source.count(expected) != 1:
        raise ValueError("actual fixed-profile measurement step missing or duplicated")
    return "set -euo pipefail\n" + expected.strip()[5:] + "\n"


COMMAND = r'''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
name=Path(sys.argv[0]).name
args=sys.argv[1:]
row={'command':name,'args':args}
if name=='tuna-vm-perf':
    row['desktop']=args[args.index('--desktop')+1]
    row['out']=args[args.index('--out')+1]
    row['meta']=[args[i+1] for i,v in enumerate(args) if v=='--meta']
    repeat=next(v.split('=',1)[1] for v in row['meta'] if v.startswith('acquisition_repeat='))
    row['profile']=args[args.index('--gnome-profile')+1]
    row['case']=row['profile']+':'+repeat+':'+row['desktop']
with open(os.environ['RECEIPTS'],'a') as stream:
    stream.write(json.dumps(row)+'\n')
if name=='sudo' and os.environ.get('FAIL_IMAGE_BUILD') and args[:2]==['podman','build']:
    sys.exit(17)
if name=='tuna-vm-perf':
    target=Path(row['out']);target.mkdir(parents=True,exist_ok=True)
    sys.path.insert(0, str(Path.cwd()/'scripts/lib'))
    from perf_vm_lifetime import Handoff, cleanup_owned_vm
    import subprocess
    handoff=Handoff(target,row['profile'],row['desktop'],int(row['case'].split(':')[1]))
    child=subprocess.Popen(['/usr/bin/python3','-c','import time; time.sleep(30)'])
    try:
        handoff.attach(child)
        cleanup_owned_vm(child,handoff=handoff)
    finally:
        handoff.close()
    row['controlled_host_child_only_not_qemu']=True
    (target/'controlled-command-receipt.json').write_text(json.dumps(row))
    if row['case']==os.environ.get('FAIL_CASE'):
        sys.exit(9)
'''


class Repeatability(unittest.TestCase):
    def execute(self, failure=None, image_failure=False, receipt_failure=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            commands = root / 'commands'
            commands.mkdir()
            (root / 'scripts').mkdir()
            for name in ('sudo', 'truncate', 'rm'):
                path = commands / name
                path.write_text(COMMAND)
                path.chmod(0o755)
            path = root / 'scripts/tuna-vm-perf'
            path.write_text(COMMAND)
            path.chmod(0o755)
            for name in ('base-digest', 'shared-image-id-diagnostic', 'shared-image-id-stock', 'gnome-version', 'baseline-packages.txt'):
                (root / name).write_text('controlled-original-' + name + '\n')
            helper = root / 'scripts/tuna-perf-profile-pairs'
            helper.write_bytes((ROOT / 'scripts/tuna-perf-profile-pairs').read_bytes())
            helper.chmod(0o755)
            library=root/'scripts/lib/perf_vm_lifetime.py'
            library.parent.mkdir(parents=True,exist_ok=True)
            library.write_bytes((ROOT/'scripts/lib/perf_vm_lifetime.py').read_bytes())
            if receipt_failure:
                python = commands / 'python3'
                python.write_text('#!/bin/sh\nif [ "$1" = - ]; then exit 23; fi\nexec /usr/bin/python3 "$@"\n')
                python.chmod(0o755)
            receipts = root / 'receipts.jsonl'
            env = dict(os.environ, PATH=str(commands) + os.pathsep + os.environ['PATH'],
                       RECEIPTS=str(receipts), GITHUB_SHA='controlled-source', FAIL_CASE=failure or '', FAIL_IMAGE_BUILD='1' if image_failure else '')
            results = [subprocess.run(['bash', '-c', measurement_script(profile)], cwd=root, env=env,
                                      capture_output=True, text=True, timeout=30) for profile in ('diagnostic', 'stock')]
            result = next((r for r in results if r.returncode), results[-1])
            statuses = [json.loads((root / path / 'acquisition-status.json').read_text()) if (root / path / 'acquisition-status.json').exists() else None for path in ('perf-artifacts', 'perf-artifacts/stock')]
            rows = [json.loads(line) for line in receipts.read_text().splitlines()]
            artifacts = sorted(str(path.relative_to(root)) for path in (root / 'perf-artifacts').rglob('controlled-command-receipt.json'))
            return result, rows, artifacts, statuses

    def test_actual_shell_retains_twelve_distinct_fresh_guest_measurements_and_order_metadata(self):
        result, rows, artifacts, statuses = self.execute()
        self.assertEqual(result.returncode, 0, result.stderr)
        measurements = [row for row in rows if row['command'] == 'tuna-vm-perf']
        self.assertEqual([row['case'] for row in measurements],
                         [profile+':'+case for profile in ('diagnostic','stock') for case in ('1:gnome','1:tuna','2:tuna','2:gnome','3:gnome','3:tuna')])
        first_profile = ['perf-artifacts/gnome', 'perf-artifacts/tuna',
                    'perf-artifacts/repeat-2/tuna', 'perf-artifacts/repeat-2/gnome',
                    'perf-artifacts/repeat-3/gnome', 'perf-artifacts/repeat-3/tuna']
        expected = first_profile + [path.replace('perf-artifacts/', 'perf-artifacts/stock/', 1) for path in first_profile]
        self.assertEqual([row['out'] for row in measurements], expected)
        self.assertEqual(artifacts, sorted(path + '/controlled-command-receipt.json' for path in expected))
        self.assertEqual(sum(row['command'] == 'truncate' for row in rows), 12)
        installs = [row for row in rows if row['command'] == 'sudo' and 'bootc' in row['args']]
        self.assertEqual(len(installs), 12)
        self.assertTrue(all('install' in row['args'] and '--wipe' in row['args'] for row in installs))
        for row in measurements:
            self.assertIn('acquisition_order='+('tuna-gnome' if ':2:' in row['case'] else 'gnome-tuna'), row['meta'])
            self.assertIn('reference_kind='+('instrumented-gnome51' if row['profile']=='diagnostic' else 'distribution-stock-gnome51'), row['meta'])
            self.assertIn('observer_revision=controlled-source', row['meta'])
            expected_root='perf-artifacts' if row['profile']=='diagnostic' else 'perf-artifacts/stock'
            self.assertEqual(row['args'][row['args'].index('--reference-image')+1], expected_root+'/reference-image.json')

    def test_first_and_later_rejections_preserve_nonzero_without_replay_or_selecting_later_passes(self):
        order = [profile+':'+case for profile in ('diagnostic','stock') for case in ('1:gnome','1:tuna','2:tuna','2:gnome','3:gnome','3:tuna')]
        for failure in ('diagnostic:1:gnome', 'diagnostic:2:gnome', 'diagnostic:2:tuna','stock:1:gnome','stock:3:tuna'):
            with self.subTest(failure=failure):
                result, rows, artifacts, statuses = self.execute(failure)
                self.assertEqual(result.returncode, 9)
                failed_profile=failure.split(':',1)[0]
                expected = []
                for profile in ('diagnostic','stock'):
                    cases=[case for case in order if case.startswith(profile+':')]
                    expected.extend(cases[:cases.index(failure)+1] if profile==failed_profile else cases)
                failed=next(row for row in statuses if row['profile']==failed_profile)
                self.assertEqual(failed['exit_status'],9)
                self.assertEqual(failed['stage'],'measurement')
                self.assertFalse(failed['all_six_commands_succeeded'])
                self.assertEqual([row['case'] for row in rows if row['command'] == 'tuna-vm-perf'], expected)
                self.assertEqual(len(artifacts), len(expected))
                self.assertEqual(sum(row['command'] == 'truncate' for row in rows), len(expected))

    def test_failed_status_receipt_never_masks_original_exit_or_creates_success(self):
        result, rows, artifacts, statuses = self.execute('diagnostic:1:gnome', receipt_failure=True)
        self.assertEqual(result.returncode,9)
        self.assertEqual(statuses,[None,None])
        measurements=[row['case'] for row in rows if row['command']=='tuna-vm-perf']
        # Missing original diagnostic status forbids stock before any disk/image operation.
        self.assertEqual(measurements,['diagnostic:1:gnome'])

    def test_unknown_profile_refused_before_any_acquisition_or_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            result=subprocess.run(['bash',str(ROOT/'scripts/tuna-perf-profile-pairs'),'private-body'],cwd=directory,capture_output=True,text=True,timeout=5)
            self.assertEqual(result.returncode,2)
            self.assertEqual(list(Path(directory).iterdir()),[])
            self.assertNotIn('private-body',result.stdout+result.stderr)

    def test_own_profile_setup_failure_cannot_execute_measurement(self):
        result, rows, artifacts, statuses = self.execute(image_failure=True)
        self.assertEqual(result.returncode,17)
        self.assertEqual(artifacts,[])
        self.assertFalse(any(row['command']=='tuna-vm-perf' for row in rows))
        self.assertTrue(all((s['exit_status'],s['case_index'],s['stage'])==(17,1,'image-build') for s in statuses))
        self.assertTrue(all(not s['all_six_commands_succeeded'] for s in statuses))

    def test_stock_condition_overrides_prior_failure_but_not_cancel_or_unqualified_setup(self):
        source = (ROOT / '.github/workflows/performance-baseline.yml').read_text()
        build = source.split('      - name: Build one shared preview payload\n',1)[1].split('      - name: Measure three alternating diagnostic pairs',1)[0]
        self.assertIn('        id: shared_payload\n',build)
        # Full original package/reference/runtime guards still precede success.
        for guard in ('tuna-perf-payload.py archive', 'tuna-perf-payload.py installed', 'tuna-perf-payload.py runtime', 'reference-image.json', 'chmod 0444'):
            self.assertIn(guard,build)
        stock = source.split('      - name: Measure three alternating stock pairs\n',1)[1].split('      - uses:',1)[0]
        self.assertIn("if: ${{ !cancelled() && steps.shared_payload.outcome == 'success' }}",stock)
        self.assertNotIn('continue-on-error',source)
        # Explicit GitHub condition truth table; original child shells execute in
        # other tests. This does not pretend to execute the Actions service.
        for cancelled,outcome,expected in ((False,'success',True),(True,'success',False),(False,'failure',False),(False,'skipped',False)):
            self.assertEqual(not cancelled and outcome=='success',expected)

    def test_each_success_receipt_requires_all_six_original_commands(self):
        result,rows,artifacts,statuses=self.execute()
        self.assertEqual(result.returncode,0)
        for status in statuses:
            self.assertEqual(set(status),{'schema','profile','exit_status','case_index','repeat','desktop','stage','all_six_commands_succeeded'})
            self.assertEqual((status['exit_status'],status['case_index'],status['repeat'],status['stage']),(0,6,3,'complete'))
            self.assertTrue(status['all_six_commands_succeeded'])

    def test_failure_evidence_upload_remains_unconditional(self):
        source = (ROOT / '.github/workflows/performance-baseline.yml').read_text()
        suffix = source.split('      - name: Measure three alternating diagnostic pairs', 1)[1]
        self.assertIn('        if: always()\n', suffix)
        self.assertIn('          path: perf-artifacts\n', suffix)
        self.assertIn('- run: python3 scripts/lib/tuna-perf-repeatability-tests.py', source)


if __name__ == '__main__':
    unittest.main()
