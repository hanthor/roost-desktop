#!/usr/bin/env python3
"""Verify retained bytes and recompute the two scoped baseline measurements."""
import hashlib
import json
import lzma
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parent
manifest = json.loads((ROOT / "manifest.json").read_text())
for item in manifest["files"]:
    data = (ROOT / item["path"]).read_bytes()
    key = "compressed_sha256" if "raw_sha256" in item else "sha256"
    assert hashlib.sha256(data).hexdigest() == item[key], item["path"]
    if "raw_sha256" in item:
        raw = lzma.decompress(data)
        assert len(raw) == item["raw_bytes"]
        assert hashlib.sha256(raw).hexdigest() == item["raw_sha256"]


def read(desktop, name):
    return json.loads(lzma.decompress((ROOT / desktop / (name + ".xz")).read_bytes()))


def percentiles(values):
    values = sorted(values)
    assert values
    return {"count": len(values), **{name: values[math.ceil(p * len(values)) - 1]
            for name, p in (("p50", .50), ("p95", .95), ("p99", .99))}}


for desktop in ("gnome", "roost"):
    report = read(desktop, "report.json")
    trace = read(desktop, "presentation.json")
    frames = sorted(trace["frames"], key=lambda row: row["commit"])
    assert len(frames) == 240 and len({row["commit"] for row in frames}) == 240
    assert trace["commits"] == len(frames) + trace["discarded"] + trace["pending"]
    intervals = [(b["presented_ns"] - a["presented_ns"]) / 1e6
                 for a, b in zip(frames, frames[1:])]
    assert all(value > 0 for value in intervals)
    cadence = percentiles(intervals)
    phases = read(desktop, "guest-phases.json")
    idle = percentiles([row["cpu_observed_percent"] for row in read(desktop, "samples.json")
                        if row.get("cpu_observed_percent") is not None
                        and row.get("cpu_interval_start_boottime_s") is not None
                        and phases["idle_start"] <= row["cpu_interval_start_boottime_s"]
                        <= row["boottime_s"] <= phases["idle_end"]])
    assert idle["count"] >= 20
    for actual, expected in ((cadence, report["presentation_cadence"]["interval_ms"]),
                             (idle, report["idle_user_cpu_observed_percent"])):
        assert all(actual[key] == expected[key] for key in actual)
    notifications = read(desktop, "notifications.json")
    ids = {row["notification_id"] for row in notifications}
    assert len(ids) == len(notifications) == report["accepted_notification_count"] == 10
    assert all(type(value) is int and 1 <= value <= 0xffffffff for value in ids)
    print(json.dumps({"desktop": desktop, "presentation_interval_ms": cadence,
                      "idle_user_cpu_percent": idle, "accepted_notifications": len(ids)}))
