"""Bounded CI guest-agent transport; only the fixed lifecycle probe is executed.

Protocol: https://www.qemu.org/docs/master/interop/qemu-ga-ref.html
"""
import base64
import binascii
import json
import socket
import time


def valid_orca_read_error(value):
    types = {"RuntimeError", "CalledProcessError", "FileNotFoundError", "ProcessLookupError",
             "PermissionError", "OSError", "ValueError", "JSONDecodeError", "UnicodeDecodeError",
             "TimeoutExpired", "StopIteration", "KeyError", "IndexError", "TypeError"}
    if not isinstance(value, dict) or not {"exception_type"} <= set(value) or not set(value) <= {"exception_type", "errno", "returncode", "dbus_method", "stderr"}:
        return False
    if not isinstance(value["exception_type"], str) or value["exception_type"] not in types:
        return False
    for key, bound in (("errno", 4095), ("returncode", 255)):
        if key in value and (type(value[key]) is not int or not -bound <= value[key] <= bound):
            return False
    if "returncode" in value and value["exception_type"] != "CalledProcessError":
        return False
    if "errno" in value and value["exception_type"] not in {"OSError", "FileNotFoundError", "ProcessLookupError", "PermissionError"}:
        return False
    if ("dbus_method" in value) != ("stderr" in value):
        return False
    if "dbus_method" in value:
        if value["exception_type"] != "CalledProcessError" or "returncode" not in value:
            return False
        if not isinstance(value["dbus_method"], str) or value["dbus_method"] not in {"GetNameOwner", "GetConnectionUnixProcessID", "GetConnectionUnixUser", "GetVersion"}:
            return False
        message = value["stderr"]
        if not isinstance(message, str) or len(message) > 512 or any(not c.isprintable() and c not in "\n\t" for c in message):
            return False
    return True


class GuestAgent:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(5)
        self.socket.connect(path)
        self.stream = self.socket.makefile("rwb", buffering=0)
        nonce = time.time_ns() % (1 << 62)
        self.stream.write(b"\xff" + json.dumps({"execute": "guest-sync-delimited",
                                               "arguments": {"id": nonce}}).encode() + b"\n")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                response = self._read()
            except json.JSONDecodeError:
                continue
            if response.get("return") == nonce:
                break
        else:
            raise RuntimeError("guest-agent synchronization timed out")

    def _read(self):
        line = self.stream.readline(1024 * 1024 + 1)
        if not line or len(line) > 1024 * 1024 or not line.endswith(b"\n"):
            raise RuntimeError("missing or oversized guest-agent response")
        return json.loads(line.lstrip(b"\xff"))

    def command(self, name, **arguments):
        self.stream.write(json.dumps({"execute": name, "arguments": arguments}).encode() + b"\n")
        result = self._read()
        if "error" in result:
            raise RuntimeError(f"guest-agent command {name} rejected")
        return result["return"]

    def run(self, action, *arguments, timeout=30):
        pid = self.command("guest-exec", path="/usr/libexec/tuna-vm-lifecycle",
                           arg=[action, *map(str, arguments)], **{"capture-output": True})["pid"]
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            status = self.command("guest-exec-status", pid=pid)
            if status["exited"]:
                if status.get("exitcode") != 0 or status.get("out-truncated") or status.get("err-truncated"):
                    if action == "orca-read" and not status.get("out-truncated"):
                        try:
                            encoded = status.get("out-data", "")
                            if not isinstance(encoded, str) or len(encoded) > 5464:
                                raise ValueError("read diagnostic exceeds transport bound")
                            output = base64.b64decode(encoded, validate=True)
                            if len(output) <= 4096:
                                diagnostic = json.loads(output).get("orca_read_error")
                            else:
                                diagnostic = None
                        except (binascii.Error, ValueError, UnicodeDecodeError, AttributeError, TypeError):
                            diagnostic = None
                        if valid_orca_read_error(diagnostic):
                            raise RuntimeError(f"guest lifecycle orca-read failed (exit={status.get('exitcode')}): {json.dumps(diagnostic)}")
                    raise RuntimeError(f"guest lifecycle {action} failed (exit={status.get('exitcode')})")
                return json.loads(base64.b64decode(status.get("out-data", ""), validate=True))
            time.sleep(0.1)
        raise RuntimeError(f"guest lifecycle {action} timed out")

    def close(self):
        self.stream.close()
        self.socket.close()
