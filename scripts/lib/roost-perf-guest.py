#!/usr/bin/env python3
"""CI-only interactive-user resource observer. No release thresholds."""
import argparse
import base64
import hashlib
import json
import os
import pwd
import re
import stat
import subprocess
import tempfile
from pathlib import Path
import time


def global_records(raw, socket_name):
    """Journal-sized chunks; a digest lets the host reject partial captures."""
    if not raw or len(raw) > 2 * 1024 * 1024:
        raise ValueError("Wayland capture is empty or exceeds 2 MiB")
    digest = hashlib.sha256(raw).hexdigest()
    parts = [base64.b64encode(raw[at:at+2048]).decode("ascii")
             for at in range(0, len(raw), 2048)]
    return [{"sha256": digest, "index": index, "count": len(parts),
             "socket": socket_name, "data": part} for index, part in enumerate(parts)]


def capture_globals(uid):
    runtime = Path(f"/run/user/{uid}")
    if not runtime.is_dir():
        return False
    for path in sorted(runtime.iterdir()):
        if not path.name.startswith(("wayland-", "roost-")):
            continue
        try:
            info = path.stat()
            if info.st_uid != uid or not stat.S_ISSOCK(info.st_mode):
                continue
            with tempfile.TemporaryFile() as output:
                result = subprocess.run([
                    "runuser", "-u", pwd.getpwuid(uid).pw_name, "--", "env",
                    f"XDG_RUNTIME_DIR={runtime}", f"WAYLAND_DISPLAY={path.name}",
                    "wayland-info"], stdout=output, stderr=subprocess.DEVNULL, timeout=15)
                output.seek(0)
                raw = output.read(2 * 1024 * 1024 + 1)
            if result.returncode != 0:
                continue
            if (b"interface: 'wl_compositor'" not in raw
                    or b"interface: 'xdg_wm_base'" not in raw):
                continue  # Ignore a private helper socket rather than mislabel it.
            marker = Path("/run/roost-perf-desktop.json")
            staged = marker.with_suffix(f".tmp-{os.getpid()}")
            staged.write_text(json.dumps({"uid": uid, "socket": path.name}))
            staged.replace(marker)
            for record in global_records(raw, path.name):
                print("roost-perf-globals: " + json.dumps(record), flush=True)
            return True
        except (FileNotFoundError, ConnectionError, subprocess.TimeoutExpired):
            continue  # The graphical session may still be starting.
    return False


def read_process(path):
    status = dict(line.split(":", 1) for line in (path / "status").read_text().splitlines() if ":" in line)
    # The command field can contain spaces and closing parentheses.
    fields = (path / "stat").read_text().rsplit(")", 1)[1].split()
    return {
        "pid": int(path.name), "uid": int(status["Uid"].split()[0]), "state": fields[0],
        "ppid": int(fields[1]), "start_ticks": int(fields[19]),
        "cpu_ticks": int(fields[11]) + int(fields[12]),
    }


def drm_clients(path):
    """Retain documented memory counters only, never arbitrary fdinfo text.

    https://www.kernel.org/doc/html/v6.9/gpu/drm-usage-stats.html
    Counters describe client accounting, not unique physical GPU allocations.
    """
    clients, errors = [], []
    try:
        files = list((path / "fdinfo").iterdir())
    except FileNotFoundError:
        return clients, errors
    except OSError as error:
        return clients, [type(error).__name__]
    for info in files:
        try:
            fields = dict(line.split(":", 1) for line in info.read_text().splitlines() if ":" in line)
        except FileNotFoundError:
            continue  # Closed FD during this non-atomic observation.
        except OSError as error:
            errors.append(type(error).__name__)
            continue
        driver = fields.get("drm-driver", "").strip()
        if not driver:
            continue
        counters = {}
        for key, value in fields.items():
            if not re.fullmatch(r"drm-(?:memory|total|shared|resident|purgeable|active)-\S+", key):
                continue
            match = re.fullmatch(r"\s*(\d+)\s*(KiB|MiB)?\s*", value)
            if not match:
                errors.append("InvalidMemoryCounter")
                continue
            counters[key] = int(match[1]) * {None: 1, "KiB": 1024, "MiB": 1024**2}[match[2]]
        client = fields.get("drm-client-id", "").strip()
        if not client.isdecimal():
            errors.append("MissingClientIdentity")
            continue  # Duplicate handles cannot safely be accounted.
        clients.append({"driver": driver, "device": fields.get("drm-pdev", "").strip(),
                        "client_id": client, "memory_bytes": counters})
    return clients, errors


def unique_drm_clients(rows):
    clients = {}
    for row in rows:
        for client in row["drm_clients"]:
            key = (client["driver"], client["device"], client["client_id"])
            if key not in clients:
                clients[key] = dict(client, memory_bytes=dict(client["memory_bytes"]))
            else:
                # Shared/duplicated FDs expose one client. Sampling is not
                # atomic, so preserve the larger observed value per counter.
                values = clients[key]["memory_bytes"]
                for name, value in client["memory_bytes"].items():
                    values[name] = max(value, values.get(name, 0))
    return list(clients.values())


def sample(uid, proc=Path("/proc"), observer=None):
    processes, errors = {}, []
    for path in proc.iterdir():
        if not path.name.isdigit():
            continue
        try:
            row = read_process(path)
            if row["uid"] == uid and row["state"] not in ("Z", "X", "x"):
                processes[row["pid"]] = row
        except (OSError, ValueError, KeyError, IndexError):
            continue  # A process can exit between directory and status reads.
    excluded = {observer} if observer is not None else set()
    while True:
        children = {pid for pid, row in processes.items() if row["ppid"] in excluded}
        if children.issubset(excluded):
            break
        excluded.update(children)
    rows = []
    for pid, row in processes.items():
        if pid in excluded:
            continue
        path = proc / str(pid)
        try:
            memory = dict(line.split(":", 1) for line in (path / "smaps_rollup").read_text().splitlines() if ":" in line)
            row.update(pss_kib=int(memory["Pss"].split()[0]), rss_kib=int(memory["Rss"].split()[0]),
                       fds=len(list((path / "fd").iterdir())))
            row["drm_clients"], row["drm_errors"] = drm_clients(path)
            current_identity = read_process(path)
            if current_identity["start_ticks"] != row["start_ticks"] or current_identity["uid"] != uid:
                errors.append({"pid": pid, "error": "ProcessIdentityChanged"})
                continue
            rows.append(row)
        except (OSError, ValueError, KeyError) as error:
            try:
                identity = read_process(path)
                if identity["state"] in ("Z", "X", "x"):
                    continue  # Exited processes can retain /proc entries until reaped.
            except (FileNotFoundError, ProcessLookupError):
                continue  # Ordinary process exit during a non-atomic sample.
            except (OSError, ValueError, KeyError, IndexError):
                pass
            errors.append({"pid": pid, "error": type(error).__name__})
    drm = unique_drm_clients(rows)
    drm_errors = [{"pid": row["pid"], "error": error} for row in rows for error in row["drm_errors"]]
    gpu = {"clients": drm, "errors": drm_errors,
           "status": "incomplete" if drm_errors else ("available" if any(c["memory_bytes"] for c in drm) else "unavailable")}
    return {"uid": uid, "processes": rows, "unreadable": errors, "drm_memory": gpu,
            "pss_kib": sum(row["pss_kib"] for row in rows),
            "rss_kib": sum(row["rss_kib"] for row in rows),
            "fds": sum(row["fds"] for row in rows), "process_count": len(rows)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--interval", type=float, default=1)
    ap.add_argument("--user", help="target interactive user; observer may run as root to read protected smaps")
    args = ap.parse_args()
    if args.interval <= 0:
        ap.error("interval must be positive")
    uid = pwd.getpwnam(args.user).pw_uid if args.user else os.getuid()
    previous, previous_at, previous_boot_at = {}, None, None
    globals_captured = False
    ticks = os.sysconf("SC_CLK_TCK")
    while True:
        start = time.monotonic()
        boot_start = time.clock_gettime(time.CLOCK_BOOTTIME)
        row = sample(uid, observer=os.getpid())
        current = {(p["pid"], p["start_ticks"]): p["cpu_ticks"] for p in row["processes"]}
        delta = sum(max(0, count - previous.get(key, count)) for key, count in current.items())
        row.update(monotonic_s=start, collection_s=time.monotonic()-start, boottime_s=time.clock_gettime(time.CLOCK_BOOTTIME),
                   hz=ticks, cpu_observed_percent=(100 * delta / ticks / (start - previous_at)
                                                  if previous_at is not None else None),
                   departed_processes=len(previous.keys() - current.keys()))
        row["cpu_interval_start_boottime_s"] = previous_boot_at
        print("roost-perf-sample: " + json.dumps(row, separators=(",", ":")), flush=True)
        previous, previous_at, previous_boot_at = current, start, boot_start
        if not globals_captured:
            globals_captured = capture_globals(uid)
        time.sleep(max(0, args.interval - (time.monotonic() - start)))


if __name__ == "__main__":
    main()
