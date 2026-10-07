#!/usr/bin/env python3
"""Exercise the actual image helper's refusal paths without building a kernel."""
import json
import os
from pathlib import Path
import subprocess
import shlex
import tempfile
import unittest

HELPER = Path(__file__).resolve().parents[2] / 'packaging/marlin/roost-image-kernels'
COMMAND = '''#!/usr/bin/env python3
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
with open(os.environ['CALLS'], 'a') as f: f.write(json.dumps([name, *sys.argv[1:]])+'\\n')
if os.environ.get('FAIL_COMMAND') == name: sys.exit(7)
if name == 'pacman':
 text = os.environ.get('CURRENT_PACKAGES', 'linux 7.2.9-arch1-1') if sys.argv[1:] == ['-Q'] else 'linux'
 sys.stdout.write(text.rstrip('\\n')+'\\n')
elif name == 'depmod':
 if os.environ.get('MISSING') != 'modules.dep':
  (pathlib.Path(os.environ['MODULES']) / sys.argv[1] / 'modules.dep').write_text('kernel/fs/ext4/ext4.ko:\\n')
elif name == 'dracut':
 pathlib.Path(sys.argv[3]).write_bytes(b'fixture initramfs, not a real boot artifact')
elif name == 'lsinitrd':
 if sys.argv[1] == '-m': print('base' if os.environ.get('MISSING') == 'bootc' else 'base\\nbootc')
 else:
  version = pathlib.Path(sys.argv[1]).parent.name
  if os.environ.get('MISSING') == 'version': version = 'wrong-version'
  print('usr/lib/modules/'+version+'/kernel/fs/ext4/ext4.ko')
  if os.environ.get('MISSING') != 'service': print('usr/lib/systemd/system/bootc-root-setup.service')
'''


class ImageKernelBuild(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.modules = self.root / 'modules'
        self.modules.mkdir()
        self.receipts = self.root / 'receipts'
        binary = self.root / 'bin'
        binary.mkdir()
        for command in ['pacman', 'depmod', 'dracut', 'lsinitrd']:
            path = binary / command
            path.write_text(COMMAND)
            path.chmod(0o755)
        self.log = self.root / 'calls.jsonl'
        self.env = dict(os.environ, PATH=str(binary)+os.pathsep+os.environ['PATH'],
                        CALLS=str(self.log), MODULES=str(self.modules))

    def kernel(self, name='7.2.9-arch1-1', image=b'fixture kernel'):
        path = self.modules / name
        path.mkdir()
        (path / 'vmlinuz').write_bytes(image)
        return path

    def run_helper(self, **environment):
        return subprocess.run(['bash', str(HELPER), 'preview', str(self.modules), str(self.receipts)],
                              env={**self.env, **environment}, capture_output=True, timeout=10)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_selected_image_kernel_and_complete_inventory_retained(self):
        kernel = self.kernel()
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        actual = self.calls()
        self.assertIn(['depmod', kernel.name], actual)
        self.assertIn(['dracut', '--force', '--no-hostonly', str(kernel / 'initramfs.img'), kernel.name], actual)
        receipts = self.receipts / 'preview'
        self.assertEqual((receipts / 'kernel-version.txt').read_text().strip(), kernel.name)
        self.assertEqual((receipts / 'kernel-package.txt').read_text().strip(), 'linux')
        self.assertIn('modules.dep', (receipts / 'module-files.txt').read_text())
        self.assertEqual(len((receipts / 'kernel-sha256.txt').read_text().splitlines()), 3)

    def test_absent_empty_or_multiple_kernel_images_refused(self):
        self.assertNotEqual(self.run_helper().returncode, 0)
        self.kernel(image=b'')
        self.assertNotEqual(self.run_helper().returncode, 0)
        (self.modules / '7.2.9-arch1-1/vmlinuz').write_bytes(b'kernel')
        self.kernel('another-kernel')
        self.assertNotEqual(self.run_helper().returncode, 0)

    def test_real_command_failure_stops_before_later_build_step(self):
        self.kernel()
        for failed, later in [('depmod', 'dracut'), ('dracut', 'lsinitrd')]:
            self.log.unlink(missing_ok=True)
            result = self.run_helper(FAIL_COMMAND=failed)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn(later, [row[0] for row in self.calls()])

    def test_missing_index_bootc_module_unit_or_selected_version_refused(self):
        kernel = self.kernel()
        for missing in ['modules.dep', 'bootc', 'service', 'version']:
            (kernel / 'modules.dep').unlink(missing_ok=True)
            with self.subTest(missing=missing):
                self.assertNotEqual(self.run_helper(MISSING=missing).returncode, 0)

    def test_unrelated_module_directory_retained_without_selecting_it(self):
        self.kernel()
        (self.modules / 'orphan-without-kernel').mkdir()
        self.assertEqual(self.run_helper().returncode, 0)
        self.assertIn('orphan-without-kernel', (self.receipts / 'preview/module-directories.txt').read_text())


class PinnedBaseline(unittest.TestCase):
    def setUp(self):
        ImageKernelBuild.setUp(self)
        self.baseline = self.root / 'baseline.txt'
        self.packages = 'linux 7.2.9-arch1-1\nmutter 51.0-1.2\n'
        self.baseline.write_text(self.packages)
        self.critical_file = self.root / 'critical.bin'
        self.critical_file.write_bytes(b'original kernel or Mutter fixture')
        import hashlib
        self.critical = self.root / 'critical.sha256'
        self.critical.write_text(hashlib.sha256(self.critical_file.read_bytes()).hexdigest()+'  '+str(self.critical_file)+'\n')
        self.guard = HELPER.with_name('roost-pinned-baseline')

    def run_guard(self, packages=None):
        return subprocess.run(['bash', str(self.guard), str(self.baseline), str(self.critical)],
                              env={**self.env, 'CURRENT_PACKAGES': self.packages if packages is None else packages},
                              capture_output=True, timeout=10)

    def test_candidate_extra_package_allowed_with_original_baseline_intact(self):
        self.assertEqual(self.run_guard(self.packages+'roost 0.1.0-1\n').returncode, 0)

    def test_missing_or_upgraded_baseline_package_refused(self):
        for current in ['linux 7.2.9-arch1-1\n', self.packages.replace('51.0-1.2', '51.0-1.3')]:
            self.assertNotEqual(self.run_guard(current).returncode, 0)

    def test_changed_or_missing_critical_kernel_or_mutter_file_refused(self):
        self.critical_file.write_bytes(b'replaced binary')
        self.assertNotEqual(self.run_guard().returncode, 0)
        self.critical_file.unlink()
        self.assertNotEqual(self.run_guard().returncode, 0)

    def test_empty_unsorted_duplicate_or_malformed_baseline_refused(self):
        for text in ['', 'mutter 51.0-1.2\nlinux 7.2.9-arch1-1\n', self.packages+self.packages,
                     'linux 7.2.9-arch1-1 extra-field\n']:
            self.baseline.write_text(text)
            self.assertNotEqual(self.run_guard().returncode, 0)

    def test_actual_preview_preflight_refuses_before_any_package_mutation(self):
        container = HELPER.with_name('Containerfile').read_text()
        body = container.split('RUN set -eux;', 1)[1].split('&& pacman -U', 1)[0].replace('\\\n', '\n')
        guard = 'bash '+shlex.quote(str(self.guard))+' '+shlex.quote(str(self.baseline))+' '+shlex.quote(str(self.critical))
        body = body.replace('/usr/libexec/roost-pinned-baseline', guard)
        self.critical_file.write_bytes(b'changed')
        result = subprocess.run(['bash', '-c', 'set -e; '+body+'\n'],
                                env={**self.env, 'CURRENT_PACKAGES': self.packages,
                                     'PERF_PINNED_BASELINE': 'true'}, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual(calls, [['pacman', '-Q']])


if __name__ == '__main__':
    unittest.main()
