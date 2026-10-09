"""Strict original Stop boundary qualification; does not alter captured frames."""
import re
from gnome_sysprof import capture_boundary_evidence, frame_source_evidence


def capture_stop_boundary_evidence(decoded, metadata, output="Virtual-1"):
    start = capture_boundary_evidence(decoded, metadata, output)
    pid = metadata["pid"]
    marks = [row for row in decoded["marks"] if row["pid"] == pid]
    rows = [row for row in marks if row["name"] == "Tuna::CaptureStopBoundary"]
    if len(rows) != 1 or metadata.get("drained_stop_required") is not True:
        raise ValueError("missing or ambiguous actual drained Stop boundary")
    requested_ns = metadata.get("stop_requested_monotonic_ns")
    stopped_ns = metadata.get("stopped_monotonic_ns")
    if (type(requested_ns) is not int or type(stopped_ns) is not int
            or not metadata["started_monotonic_ns"] <= requested_ns <= stopped_ns):
        raise ValueError("invalid original Stop request/return receipt")
    row = rows[0]
    match = re.fullmatch(r"output=([^ ]+) clock=(0x[0-9a-f]+) next=(\d+) pending=(\d+) depth=(\d+) "
                         r"dispatched=(\d+) state=([^ ]+) requested_us=(\d+) closing_us=(\d+) samples=(\d+)", row["message"])
    if not match:
        raise ValueError("malformed actual drained Stop boundary")
    name, clock, counter, pending, depth, dispatched, state, requested, closing, samples = match.groups()
    counter, pending, depth, dispatched, requested, closing, samples = map(int, (counter,pending,depth,dispatched,requested,closing,samples))
    if (name != output or clock != start["clock"] or pending or depth or dispatched
            or state not in ("init","idle","scheduled","scheduled-now","scheduled-later")
            or not 1 <= samples <= 5000 or not 0 <= closing-requested < 5_000_000
            or not requested_ns // 1000 <= requested <= closing <= row["monotonic_ns"] // 1000
            or row["monotonic_ns"] > stopped_ns):
        raise ValueError("original clock was not drained before Stop acknowledgement")
    sources = frame_source_evidence(decoded, pid)
    dispatches = sources["dispatches"]
    if (not dispatches or dispatches[-1]["output"] != output
            or counter != dispatches[-1]["frame_counter"] + 1):
        raise ValueError("Stop counter lacks its last original dispatch owner")
    scopes = [item for item in marks if item["name"] in
              ("Clutter::FrameClock::dispatch()", "Clutter::FrameClock::presented()", "Clutter::FrameClock::ready()")]
    if any(item["monotonic_ns"] + item["duration_ns"] > row["monotonic_ns"] for item in scopes):
        raise ValueError("original frame scope crosses the Stop boundary")
    kinds = [item["name"] for item in scopes]
    dispatched_count = kinds.count("Clutter::FrameClock::dispatch()")
    completion_count = kinds.count("Clutter::FrameClock::presented()") + kinds.count("Clutter::FrameClock::ready()")
    if dispatched_count != completion_count:
        raise ValueError("original Stop capture still has incomplete frame ownership")
    return {"boundary_mark": row, "output": name, "clock": clock,
            "next_counter": counter, "requested_us": requested, "closing_us": closing,
            "samples": samples, "last_source_dispatch": dispatches[-1],
            "dispatch_count": dispatched_count, "completion_count": completion_count,
            "limitation": "Actual acquisition boundary only; no frame trimming, clock substitution or kernel causal join."}
