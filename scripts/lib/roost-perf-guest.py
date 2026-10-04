#!/usr/bin/env python3
"""CI-only interactive-user resource observer. No release thresholds."""
import argparse
import json
import os
import pwd
from pathlib import Path
import time


def read_process(path):
    status = dict(line.split(":", 1) for line in (path / "status").read_text().splitlines() if ":" in line)
    # The command field can contain spaces and closing parentheses.
    fields = (path / "stat").read_text().rsplit(")", 1)[1].split()
    return {
        "pid": int(path.name), "uid": int(status["Uid"].split()[0]),
        "ppid": int(fields[1]), "start_ticks": int(fields[19]),
        "cpu_ticks": int(fields[11]) + int(fields[12]),
    }


def sample(uid, proc=Path("/proc"), observer=None):
    processes, errors = {}, []
    for path in proc.iterdir():
        if not path.name.isdigit():
            continue
        try:
            row = read_process(path)
            if row["uid"] == uid:
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
            current_identity = read_process(path)
            if current_identity["start_ticks"] != row["start_ticks"] or current_identity["uid"] != uid:
                errors.append({"pid": pid, "error": "ProcessIdentityChanged"})
                continue
            rows.append(row)
        except (OSError, ValueError, KeyError) as error:
            errors.append({"pid": pid, "error": type(error).__name__})
    return {"uid": uid, "processes": rows, "unreadable": errors,
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
    previous, previous_at = {}, None
    ticks = os.sysconf("SC_CLK_TCK")
    while True:
        start = time.monotonic()
        row = sample(uid, observer=os.getpid())
        current = {(p["pid"], p["start_ticks"]): p["cpu_ticks"] for p in row["processes"]}
        delta = sum(max(0, count - previous.get(key, count)) for key, count in current.items())
        row.update(monotonic_s=start, boottime_s=time.clock_gettime(time.CLOCK_BOOTTIME),
                   hz=ticks, cpu_observed_percent=(100 * delta / ticks / (start - previous_at)
                                                  if previous_at is not None else None),
                   departed_processes=len(previous.keys() - current.keys()))
        print("roost-perf-sample: " + json.dumps(row, separators=(",", ":")), flush=True)
        previous, previous_at = current, start
        time.sleep(max(0, args.interval - (time.monotonic() - start)))


if __name__ == "__main__":
    main()
