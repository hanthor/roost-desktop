#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import json
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader("vm_boot_lane", str(Path(__file__).resolve().parents[1] / "roost-vm-lane"))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class BootApi(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
