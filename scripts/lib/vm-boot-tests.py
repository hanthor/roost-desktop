#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import ast
import json
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

loader = importlib.machinery.SourceFileLoader("vm_boot_lane", str(Path(__file__).resolve().parents[1] / "roost-vm-lane"))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class OrcaLifecycleAuthority(unittest.TestCase):
    def test_negative_control_consumes_actual_inventory_identity_contract(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        producer = next(node for node in tree.body
                        if isinstance(node, ast.FunctionDef) and node.name == "inventory")
        record = next(node for node in ast.walk(producer) if isinstance(node, ast.Dict)
                      and any(isinstance(key, ast.Constant) and key.value == "start_ticks"
                              for key in node.keys))
        stat = ["0"] * 20
        stat[19] = "18000"
        actual = eval(compile(ast.Expression(record), str(source), "eval"), {
            "path": SimpleNamespace(name="874"), "uid": 1000,
            "executable": "roost-compositor", "stat": stat})
        consumer = next(node for node in tree.body
                        if isinstance(node, ast.FunctionDef) and node.name == "owned_compositor")
        scope = {"OWNER": SimpleNamespace(pw_uid=1000)}
        exec(compile(ast.Module(body=[consumer], type_ignores=[]), str(source), "exec"), scope)
        select = scope["owned_compositor"]
        self.assertIs(select([actual, {**actual, "executable": "roost-shell-gtk"}]), actual)
        for invalid in ([], [{**actual, "uid": 1001}], [actual, {**actual, "pid": 875}]):
            with self.assertRaises(RuntimeError):
                select(invalid)
        # This verifies producer/consumer shape and refusal policy. Only the
        # genuine VM can qualify actual bus credentials and reader exclusion.

    def test_compositor_authority_comes_from_actual_backend(self):
        source = (Path(__file__).resolve().parents[2] / "crates/compositor/src/runtime.rs").read_text()
        self.assertIn("let hardware = matches!(self.backend, Backend::Drm(_));", source)
        self.assertIn("self.orca.set_enabled(enabled && hardware);", source)
        self.assertIn("let hardware = false;", source)
        self.assertTrue('doc["screen_reader_pid"] = serde_json::json!(self.orca.pid());' in source)

    def test_no_global_orca_service_control_or_replace(self):
        root = Path(__file__).resolve().parents[2]
        compositor = (root / "crates/compositor/src/orca.rs").read_text()
        shell = (root / "crates/shell-gtk/src/orca.rs").read_text()
        self.assertNotIn('"--replace"', compositor)
        self.assertNotIn('"orca.service"', compositor)
        self.assertIn('Command::new("/usr/bin/orca")', compositor)
        self.assertIn('libc::PR_SET_PDEATHSIG, libc::SIGTERM', compositor)
        self.assertIn('libc::getppid() != parent', compositor)
        self.assertNotIn('gio::bus_get_future', shell)
        self.assertIn('control.set_screen_reader(enabled)', shell)


class BootApi(unittest.TestCase):
    def test_actual_orca_commands_use_distinct_qmp_qcodes_for_chords(self):
        tree = ast.parse((Path(__file__).resolve().parents[1] / "roost-vm-lane").read_text())
        command = next(node for node in ast.walk(tree)
                       if isinstance(node, ast.FunctionDef) and node.name == "native_command")
        chords = [tuple(ast.literal_eval(arg) for arg in node.args)
                  for node in ast.walk(command) if isinstance(node, ast.Call)
                  and isinstance(node.func, ast.Attribute) and node.func.attr == "keys"]
        self.assertIn(("insert", "h"), chords)
        self.assertIn(("shift", "f4"), chords)
        for chord in chords:
            self.assertTrue(set(chord) <= {"insert", "h", "shift", "f4", "esc"})
            qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
            qmp.file = io.StringIO()
            qmp._read = lambda: {"return": {}}
            with patch.object(lane.baseline.time, "sleep"):
                qmp.keys(*chord)
            wire = json.loads(qmp.file.getvalue())
            self.assertEqual(wire["execute"], "send-key")
            self.assertEqual(wire["arguments"]["keys"],
                             [{"type": "qcode", "data": name} for name in chord])

    def test_qmp_command_accepts_protocol_name_argument(self):
        qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
        qmp.file = io.StringIO()
        qmp._read = lambda: {"return": {}}
        with tempfile.TemporaryDirectory() as out:
            lane.Recorder(qmp, out)
            qmp.cmd("trace-event-set-state", name="virtio_gpu_cmd_set_scanout", enable=True)
        wire = json.loads(qmp.file.getvalue())
        self.assertEqual(wire, {"execute": "trace-event-set-state", "arguments": {
            "name": "virtio_gpu_cmd_set_scanout", "enable": True}})

    def test_existing_four_value_performance_call_keeps_original_devices(self):
        with patch.object(lane.os.path, "exists", return_value=True), patch.object(lane.os, "access", return_value=True), patch.object(lane.subprocess, "Popen") as spawn:
            process, qmp, serial, kvm = lane.boot("disk.raw", "/out", "/scratch", 30)
            self.assertIs(process, spawn.return_value)
            self.assertEqual((qmp, serial, kvm), ("/scratch/qmp.sock", "/out/serial.log", True))
            command = spawn.call_args.args[0]
            self.assertIn("-enable-kvm", command)
            self.assertEqual(command[command.index("-m") + 1], "6144")
            self.assertEqual(command[command.index("-smp") + 1], "4")
            self.assertIn("pcie-root-port,id=roost-gpu-port,chassis=1,slot=1", command)
            self.assertIn("virtio-vga,bus=roost-gpu-port,x-pcie-pm-no-soft-reset=on,xres=1280,yres=800", command)
            shared = lane.baseline.gpu_devices()
            offset = command.index(shared[1]) - 1
            self.assertEqual(command[offset:offset + len(shared)], shared)
            self.assertNotIn("virtio-serial-pci", command)
            self.assertFalse(any("guest_agent" in value for value in command))

    def test_explicit_guest_agent_adds_transport_and_returns_socket(self):
        with patch.object(lane.os.path, "exists", return_value=True), patch.object(lane.os, "access", return_value=True), patch.object(lane.subprocess, "Popen") as spawn:
            process, qmp, serial, kvm, agent = lane.boot("disk.raw", "/out", "/scratch", 30, guest_agent=True)
            self.assertIs(process, spawn.return_value)
            self.assertEqual(agent, "/scratch/qga.sock")
            command = spawn.call_args.args[0]
            self.assertIn("virtio-serial-pci", command)
            self.assertIn("virtserialport,chardev=roost-qga,name=org.qemu.guest_agent.0", command)


class InputProbePlacement(unittest.TestCase):
    def test_click_uses_placement_after_first_buffer(self):
        class FrameReached(Exception):
            pass

        clicks = []
        inventory_calls = []
        provisional = {"state": {"windows": [
            {"app_id": "org.roost.VmInput", "rect": [0, 0, 800, 600]}]}}
        placed = {"state": {"windows": [
            {"app_id": "org.roost.VmInput", "rect": [500, 250, 400, 240]}]}}

        def run(action):
            if action == "inventory":
                inventory_calls.append(action)
                return provisional if len(inventory_calls) == 1 else placed
            if action == "input-ready":
                return {"pid": 42, "count": 0, "mapped": True,
                        "active": bool(clicks), "width": 400, "height": 240}
            self.assertEqual(action, "start-input-probe")
            return {"started": True}

        def frame(_path):
            raise FrameReached()

        qmp = SimpleNamespace(click=lambda x, y: clicks.append((x, y)), frame=frame)
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaises(FrameReached):
                lane.run_lifecycle(qmp, SimpleNamespace(run=run), out, "/unused", lambda *_: None)
            self.assertEqual(clicks, [(600, 350)])
            mapped = json.loads((Path(out) / "lifecycle/input-mapped.json").read_text())
            self.assertEqual(mapped, placed)
            ready = json.loads((Path(out) / "lifecycle/input-ready.json").read_text())
            self.assertTrue(ready["active"])


class RepeatedCoverage(unittest.TestCase):
    def run_boots(self, root, manifests, returncodes=None):
        next_boot = iter(zip(manifests, returncodes or [0] * len(manifests)))

        def boot(command, check):
            self.assertFalse(check)
            manifest, code = next(next_boot)
            destination = Path(command[command.index("--out") + 1])
            destination.mkdir(parents=True, exist_ok=True)
            (destination / "manifest.json").write_text(json.dumps({"assertions": manifest}))
            return SimpleNamespace(returncode=code)

        args = SimpleNamespace(out=str(root), boots=5, disk="disk.raw", timeout=30,
                               tour=True, meta=[], orca_contract="distribution")
        with patch.object(lane.subprocess, "run", side_effect=boot):
            lane.repeat_boots(args)

    def test_final_lifecycle_check_does_not_claim_five_observations(self):
        startup = {"V-DRM": {"pass": True}}
        full = {**startup, "V-SUSPEND": {"pass": True}}
        with tempfile.TemporaryDirectory() as root:
            self.run_boots(root, [startup] * 4 + [full])
            proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
            self.assertEqual(proof["assertion_coverage"]["V-DRM"]["passed_boots"], [1, 2, 3, 4, 5])
            self.assertEqual(proof["assertion_coverage"]["V-SUSPEND"]["observed_boots"], [5])
            lines = (Path(root) / "assertions.txt").read_text().splitlines()
            self.assertTrue(any(line.startswith("V-SUSPEND pass observed on 1/5") for line in lines))

    def test_missing_baseline_check_is_fatal_and_retained(self):
        startup = {"V-DRM": {"pass": True}}
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaisesRegex(SystemExit, "repeated boot gate failed"):
                self.run_boots(root, [startup, {}, startup, startup, startup])
            proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
            self.assertEqual(proof["assertion_coverage"]["V-DRM"]["missing_boots"], [2])
            self.assertIn("V-DRM fail observed on 4/5", (Path(root) / "assertions.txt").read_text())

    def test_observed_failure_and_failed_process_each_remain_fatal(self):
        startup = {"V-DRM": {"pass": True}}
        for last, codes in [({"V-DRM": {"pass": False}}, [0] * 5),
                            (startup, [0, 0, 0, 0, 1])]:
            with self.subTest(last=last, codes=codes), tempfile.TemporaryDirectory() as root:
                with self.assertRaisesRegex(SystemExit, "repeated boot gate failed"):
                    self.run_boots(root, [startup] * 4 + [last], codes)
                proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
                self.assertEqual(proof["boot_results"][-1]["returncode"], codes[-1])
                self.assertIn("V-REPEAT fail", (Path(root) / "assertions.txt").read_text())


class PciCapabilities(unittest.TestCase):
    def probe(self, pm_control, pcie=True, cycle=False):
        helper = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(helper.read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                        and node.name == "gpu_pci_caps")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            drm = root / "drm"
            pci = root / "0000:01:00.0"
            (drm / "card1").mkdir(parents=True)
            pci.mkdir()
            (drm / "card1/device").symlink_to(pci, target_is_directory=True)
            data = bytearray(256)
            data[0x34] = 0x40
            data[0x40] = 1
            data[0x41] = 0x50 if pcie else (0x40 if cycle else 0)
            data[0x44:0x46] = pm_control.to_bytes(2, "little")
            if pcie:
                data[0x50] = 0x10
                data[0x51] = 0x40 if cycle else 0
            (pci / "config").write_bytes(data)
            namespace = {"pathlib": SimpleNamespace(Path=lambda path: drm)}
            exec(compile(ast.Module(body=[function], type_ignores=[]), str(helper), "exec"), namespace)
            return namespace["gpu_pci_caps"]()["card1"]

    def test_actual_no_soft_reset_and_pcie_are_reported(self):
        caps = self.probe(8)
        self.assertTrue(caps["no_soft_reset"])
        self.assertEqual(caps["power_state"], 0)
        self.assertEqual((caps["pm_capability"], caps["pcie_capability"]), (0x40, 0x50))
        self.assertEqual(caps["slot"], "0000:01:00.0")

    def test_d3hot_is_distinct_from_no_soft_reset(self):
        caps = self.probe(3)
        self.assertFalse(caps["no_soft_reset"])
        self.assertEqual(caps["power_state"], 3)

    def test_pm_without_pcie_cannot_masquerade_as_supported_topology(self):
        self.assertEqual(self.probe(8, pcie=False)["pcie_capability"], 0)

    def test_capability_cycle_is_bounded(self):
        self.assertTrue(self.probe(8, cycle=True)["no_soft_reset"])


if __name__ == "__main__":
    unittest.main()
