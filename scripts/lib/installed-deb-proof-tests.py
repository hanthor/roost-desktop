#!/usr/bin/env python3
"""Source-policy negatives for real package scripts; no product build/runtime."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
import io
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('installed', ROOT / 'scripts/lib/installed-deb-proof.py')
M = importlib.util.module_from_spec(spec)
spec.loader.exec_module(M)


class InstalledPolicy(unittest.TestCase):
    def process_case(self, inode=20, replacement=None, changed_field=None):
        original = {'sha256': 'a' * 64, 'dev': 1, 'ino': 20, 'size': 100}
        executed = SimpleNamespace(st_dev=1, st_ino=inode, st_size=100,
                                   st_uid=0, st_mode=0o100755, st_mtime_ns=10, st_ctime_ns=11)
        current = SimpleNamespace(**vars(executed))
        if replacement is not None:
            current.st_ino = replacement
        tail = ['S', '1'] + ['0'] * 17 + ['123'] + ['0'] * 4
        reads = {'stat': 0, 'status': 0}
        def text(path, *args, **kwargs):
            key = path.name
            reads[key] += 1
            if key == 'stat':
                after = tail[:]
                if reads[key] == 2 and changed_field in ('start', 'parent'):
                    after[19 if changed_field == 'start' else 1] = '999'
                return '99 (roost-compositor) ' + ' '.join(after)
            if key == 'status':
                uid = 999 if reads[key] == 2 and changed_field == 'uid' else 1000
                return 'Uid:\t' + '\t'.join([str(uid)] * 4) + '\n'
            raise AssertionError('unexpected process read')
        links = ['/usr/bin/roost-compositor'] * 2
        if changed_field == 'exe':
            links[1] = '/usr/bin/replaced'
        with patch.object(Path, 'read_text', text), patch.object(os, 'getuid', return_value=1000), \
             patch.object(os, 'readlink', side_effect=links), patch.object(os, 'open', return_value=50) as opened, \
             patch.object(os, 'fstat', return_value=executed), patch.object(os, 'stat', return_value=current), \
             patch.object(os, 'close') as closed, patch.object(M, 'digest', return_value=original) as digest:
            try:
                return M.process(99, 'roost-compositor', {'roost-compositor': original})
            finally:
                opened.assert_called_once_with(Path('/proc/99/exe'), os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
                closed.assert_called_once_with(50)
                if inode != original['ino']:
                    digest.assert_not_called()

    def test_original_actual_executable_fd_bound_to_preflight_inode(self):
        self.assertEqual(self.process_case()['binary']['ino'], 20)

    def test_same_path_hash_different_executed_inode_rejected(self):
        with self.assertRaisesRegex(ValueError, 'executed-file-identity'):
            self.process_case(inode=21)

    def test_original_executable_replaced_during_observation_rejected(self):
        with self.assertRaisesRegex(ValueError, 'executed-file-replaced'):
            self.process_case(replacement=21)

    def test_post_hash_uid_start_parent_and_exe_change_rejected(self):
        for field in ('uid', 'start', 'parent', 'exe'):
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'process-replaced'):
                self.process_case(changed_field=field)

    def test_bounded_command_nonzero(self):
        with self.assertRaises(subprocess.CalledProcessError):
            M.run(['/bin/sh', '-c', 'printf private; exit 9'])

    def test_bounded_command_noise(self):
        with self.assertRaisesRegex(ValueError, 'command-output-limit'):
            M.run(['/usr/bin/python3', '-c', 'print("x"*65537)'])

    def test_override_refused_before_package_commands(self):
        with patch.object(os, 'getuid', return_value=1000), patch.dict(os.environ, {'ROOST_SHELL_BIN': '/tmp/wrong'}), patch.object(M, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'route-override'):
                M.preflight()
            run.assert_not_called()

    def test_uid_zero_refused(self):
        with patch.object(os, 'getuid', return_value=0), patch.object(M, 'run') as run:
            with self.assertRaises(ValueError):
                M.preflight()
            run.assert_not_called()

    def test_pid_bool_refused(self):
        with self.assertRaisesRegex(ValueError, 'pid-type'):
            M.process(True, 'roost-compositor', {})

    def test_unowned_library_fatal(self):
        with patch.object(M, 'run', side_effect=subprocess.CalledProcessError(1, ['dpkg-query'])):
            with self.assertRaisesRegex(ValueError, 'unowned-library'):
                M.owner('/usr/lib/fixture.so')

    def test_receipt_bound(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'receipt'
            path.write_bytes(b' ' * (1024 * 1024 + 1))
            with self.assertRaisesRegex(ValueError, 'receipt-limit'):
                M.bounded_json(path)

    def test_mapped_inode_mismatch_rejected_before_hashing(self):
        maps = '1-2 r-xp 0 01:01 3 /usr/lib/libgtk-4.so.1\n'
        with patch('builtins.open', return_value=io.BytesIO(maps.encode())), patch.object(Path, 'resolve', lambda p, **_: p), patch.object(os, 'stat', return_value=SimpleNamespace(st_dev=257, st_ino=4)), patch.object(M, 'digest') as digest:
            with self.assertRaisesRegex(ValueError, 'mapped-file-identity'):
                M.libraries(100)
            digest.assert_not_called()

    def test_unpacked_usr_local_library_cannot_qualify(self):
        maps = '1-2 r-xp 0 01:01 3 /usr/local/lib/libgtk4-layer-shell.so.0\n'
        with patch('builtins.open', return_value=io.BytesIO(maps.encode())):
            with self.assertRaisesRegex(ValueError, 'foreign-loaded-library'):
                M.libraries(100)

    def test_companion_wrong_commit_refused(self):
        def command(args):
            if args == ['dpkg', '--verify', 'roost']:
                return ''
            if args[0] == 'git':
                return 'a' * 40
            if args[0].startswith('/usr/bin/roost-'):
                return Path(args[0]).name + ' 0.1.0'
            return '0.1.0'
        with patch.object(os, 'getuid', return_value=1000), patch.dict(os.environ, {}, clear=True), patch.object(Path, 'read_text', return_value='ID=ubuntu\nVERSION_ID="26.04"\n'), patch.object(M, 'run', side_effect=command), patch.object(M, 'owner', return_value={'name': 'roost', 'version': '0.1.0'}), patch.object(M, 'digest', return_value={'sha256': '0' * 64}), patch.object(M, 'bounded_json', return_value={'commit': 'b' * 40, 'files': {'icon-probe': '0' * 64, 'shortcut-inhibit-client': '0' * 64}}):
            with self.assertRaisesRegex(ValueError, 'companion-source-contract'):
                M.preflight()

    def test_original_session_and_replacement_contract(self):
        base = {'files': {}}
        compositor = {'pid': 100, 'exe': '/usr/bin/roost-compositor'}
        shell = {'pid': 101, 'parent': 100}
        sockets = {n: {'pid': 100} for n in ('owned', 'roost-owned.control')}
        previous = {'compositor': compositor, 'shell': shell, 'sockets': sockets}
        def proc(pid, *_):
            return compositor if pid == 100 else shell
        with patch.object(M, 'process', side_effect=proc), patch.object(Path, 'read_text', return_value='101'), patch.object(os, 'readlink', return_value='/usr/bin/roost-shell-gtk'), patch.object(M, 'peer', return_value={'pid': 100}), patch.object(M, 'libraries', return_value=[]), patch.dict(os.environ, {'XDG_RUNTIME_DIR': '/tmp/owned'}):
            self.assertEqual(M.observe(100, 'owned', base, previous)['shell'], shell)
            with self.assertRaisesRegex(ValueError, 'shell-replacement-contract'):
                M.observe(100, 'owned', base, previous, replacement=True)
            old = dict(previous, compositor={'pid': 99})
            with self.assertRaisesRegex(ValueError, 'original-session-replaced'):
                M.observe(100, 'owned', base, old)
            shell['pid'] = 102
            with patch.object(M, 'process', side_effect=lambda pid, *_: compositor if pid == 100 else shell):
                prior = dict(previous, shell={'pid': 101, 'parent': 100})
                with self.assertRaisesRegex(ValueError, 'shell-replacement-contract'):
                    M.observe(100, 'owned', base, prior)
                self.assertEqual(M.observe(100, 'owned', base, prior, True)['shell']['pid'], 102)


class PackagePolicy(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.tools = self.dir / 'tools'
        self.tools.mkdir()
        self.stage = self.dir / 'stage'
        self.stage.mkdir()
        for name in M.BINS:
            p = self.stage / 'usr/bin' / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text('#!/bin/sh\nprintf "%s\\n" "' + name + ' 0.1.0"\n')
            p.chmod(0o755)
        for name in ('usr/share/wayland-sessions/roost.desktop', 'usr/lib/systemd/user/roost-session.target', 'etc/pam.d/roost-lock'):
            p = self.stage / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text('fixture\n')
        self.env = dict(os.environ, PATH=str(self.tools) + ':' + os.environ['PATH'], FIXTURE=str(self.stage))
        self.tool('dpkg-deb', '''case "$1" in
-c) find "$FIXTURE" -type f | while read -r path; do echo "-rwxr-xr-x $path"; done ;;
-f) case "$3" in Version) echo 0.1.0 ;; Depends) echo 'libgtk4-layer-shell0 (>= 1.1), libgtk-4-1 (>= 4.12), libadwaita-1-0 (>= 1.0)' ;; esac ;;
-x) cp -R "$FIXTURE/." "$3/" ;;
--ctrl-tarfile) tar -C "$FIXTURE" -cf - ./conffiles ;;
esac''')
        (self.stage / 'conffiles').write_text('/etc/pam.d/roost-lock\n')
        self.tool('ldd', 'echo "libc.so.6 => /lib/libc.so.6 (0)"')
        self.tool('desktop-file-validate', 'exit 0')

    def tool(self, name, body):
        path = self.tools / name
        path.write_text('#!/bin/sh\nset -eu\n' + body + '\n')
        path.chmod(0o755)

    def check(self):
        return subprocess.run(['sh', str(ROOT / 'scripts/check-release-package'), 'controlled.deb'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10).returncode

    def test_positive_controlled_tool_policy(self):
        self.assertEqual(self.check(), 0)

    def test_missing_preferred_gtk(self):
        (self.stage / 'usr/bin/roost-shell-gtk').unlink()
        self.assertNotEqual(self.check(), 0)

    def test_each_binary_mode_required(self):
        (self.stage / 'usr/bin/roost-shell-gtk').chmod(0o644)
        self.assertNotEqual(self.check(), 0)

    def test_version_exit_not_hidden_by_pipeline(self):
        (self.stage / 'usr/bin/roost-shell-gtk').write_text('#!/bin/sh\necho "roost-shell-gtk 0.1.0"\nexit 9\n')
        self.assertNotEqual(self.check(), 0)

    def test_noisy_or_wrong_whole_line(self):
        for value in ('roost-shell-gtk 0.1.0 [fixture]', 'wrong-name 0.1.0', 'roost-shell-gtk 0.1.0\nextra', 'roost-shell-gtk 0.1.0\n'):
            (self.stage / 'usr/bin/roost-shell-gtk').write_text('#!/bin/sh\nprintf "%s\\n" "' + value + '"\n')
            self.assertNotEqual(self.check(), 0)

    def test_missing_loaded_elf_dependency(self):
        self.tool('ldd', 'echo "libgtk4-layer-shell.so.0 => not found"')
        self.assertNotEqual(self.check(), 0)

    def test_suffixed_floor_does_not_match(self):
        path = self.tools / 'dpkg-deb'
        path.write_text(path.read_text().replace('libgtk4-layer-shell0 (>= 1.1)', 'not-libgtk4-layer-shell0 (>= 1.1)'))
        self.assertNotEqual(self.check(), 0)

    def test_launcher_override_denies_before_any_process(self):
        result = subprocess.run(['sh', str(ROOT / 'scripts/lib/roost-proof-session-launch'), '1', str(ROOT), 'owned'], env=dict(self.env, ROOST_SHELL_BIN='/tmp/wrong'), timeout=5)
        self.assertNotEqual(result.returncode, 0)


class ReleaseDelegatePolicy(unittest.TestCase):
    def run_release(self, failure='', version_exit=0):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            tools = root / 'tools'
            tools.mkdir()
            def tool(name, body):
                path = tools / name
                path.write_text('#!/bin/sh\nset -eu\n' + body + '\n')
                path.chmod(0o755)
            tool('git', 'echo v0.1.0')
            tool('cargo', 'test "$*" = "build --locked --release -p roost-compositor -p roost-shell-gtk -p roost-shell-host -p roost-greeter"')
            tool('dpkg-shlibdeps', 'exit 9' if failure == 'shlibs' else ('echo unknown' if failure == 'malformed' else 'echo "shlibs:Depends=libc6 (>= 2.36)"'))
            tool('dpkg-deb', 'test "$1" = --build; cp "$2/DEBIAN/control" "$OUTPUT"')
            for name in M.BINS:
                path = root / 'target/release' / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('#!/bin/sh\necho "' + name + ' 0.1.0"\nexit ' + str(version_exit if name == 'roost-shell-gtk' else 0) + '\n')
                path.chmod(0o755)
            for name in ('share/wayland-sessions/roost.desktop', 'share/systemd/user/roost-session.target', 'share/pam.d/roost-lock'):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('fixture\n')
            output = root / 'control'
            result = subprocess.run(['sh', str(ROOT / 'scripts/roost-release'), '--allow-dirty', '--out', str(root/'out')], cwd=root, env=dict(os.environ, PATH=str(tools)+':'+os.environ['PATH'], OUTPUT=str(output)), stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
            return result.returncode, output.read_text() if output.exists() else None

    def test_real_release_delegates_locked_all_six_and_abi_closure(self):
        status, control = self.run_release()
        self.assertEqual(status, 0)
        self.assertIn('libc6 (>= 2.36)', control)
        self.assertIn('libgtk4-layer-shell0 (>= 1.1)', control)

    def test_shlibs_failure_or_unknown_output_cannot_package(self):
        for failure in ('shlibs', 'malformed'):
            status, control = self.run_release(failure)
            self.assertNotEqual(status, 0)
            self.assertIsNone(control)

    def test_release_version_failure_preserves_fail_closed(self):
        status, control = self.run_release(version_exit=9)
        self.assertNotEqual(status, 0)
        self.assertIsNone(control)


if __name__ == '__main__':
    unittest.main()
