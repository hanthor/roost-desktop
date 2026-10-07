"""Strict structural decoding of Sysprof's documented version-1 capture marks.

The raw capture remains authoritative. This decodes marks, not causal latency.
Layout: GNOME/sysprof 51.0 src/libsysprof-capture/sysprof-capture-types.h.
"""
import struct

MAX_CAPTURE = 8 * 1024 * 1024


def required_scope_counts(decoded, pid):
    names = [row["name"] for row in decoded["marks"] if row["pid"] == pid]
    # Sysprof stores at most 39 name bytes; use the actual ABI truncation.
    required = ("Clutter::Stage::process_queued_events#event()",
                "Clutter::FrameClock::dispatch()", "Clutter::FrameClock::presented()")
    return {name[:39]: sum(item == name[:39] for item in names) for name in required}


def decode(raw):
    if not 256 <= len(raw) <= MAX_CAPTURE:
        raise ValueError("invalid Sysprof capture size")
    if raw[:4] != struct.pack("<I", 0xFDCA975E) or raw[4] != 1 or raw[5] & 1 != 1:
        raise ValueError("expected little-endian version-1 Sysprof capture")
    marks = []
    frames = 0
    offset = 256
    while offset < len(raw):
        if len(raw) - offset < 24:
            raise ValueError("truncated Sysprof frame header")
        length, cpu, pid, clock, kind, _ = struct.unpack_from("<HhiqII", raw, offset)
        if length < 24 or length % 8 or offset + length > len(raw):
            raise ValueError("invalid or truncated Sysprof frame")
        frame = raw[offset:offset + length]
        # Unknown frame types are structurally checked and retained in raw data.
        if kind & 255 == 10:
            if length < 104 or clock < 0:
                raise ValueError("invalid Sysprof mark")
            duration = struct.unpack_from("<q", frame, 24)[0]
            if duration < 0:
                raise ValueError("negative Sysprof mark duration")
            fields = []
            for text in (frame[32:56], frame[56:96], frame[96:]):
                if b"\0" not in text:
                    raise ValueError("unterminated Sysprof mark string")
                fields.append(text.split(b"\0", 1)[0].decode("utf-8", errors="strict"))
            marks.append(dict(cpu=cpu, pid=pid, monotonic_ns=clock, duration_ns=duration,
                              group=fields[0], name=fields[1], message=fields[2]))
        frames += 1
        offset += length
    if not marks:
        raise ValueError("Sysprof capture has no marks")
    return {"frame_count": frames, "marks": marks,
            "limitation": "Raw GNOME scope marks; no input-to-presentation association inferred."}


class FrameOwnershipError(ValueError):
    """Rejected frame evidence; these bounds never become qualified latency."""

    def __init__(self, reason, evidence):
        super().__init__(reason)
        self.evidence = evidence


def overview_frame_bounds(decoded, pid, expected_inputs=20, output="Virtual-1"):
    """Reconstruct Mutter 51's two presentation slots, including newest aborts.

    This is limited to the isolated, single-output Super-tap workload. It returns
    bounds from event handling to the first owned frame dispatched after that
    handler. The event scope cannot identify the precise accepted-toggle instant.
    """
    import bisect
    import re

    if type(pid) is not int or pid <= 0 or type(expected_inputs) is not int or not 1 <= expected_inputs <= 64:
        raise ValueError("invalid overview trace identity or input count")
    marks = [row for row in decoded["marks"] if row["pid"] == pid]
    kinds = {"Clutter::FrameClock::dispatch()": "dispatch",
             "Clutter::FrameClock::presented()": "presented",
             "Clutter::FrameClock::ready()": "ready"}
    events = sorted((row for row in marks if row["name"] in kinds),
                    key=lambda row: (row["monotonic_ns"], -row["duration_ns"]))
    dispatches = [row for row in events if kinds[row["name"]] == "dispatch"]
    starts = [row["monotonic_ns"] for row in dispatches]
    if not dispatches or any(a["monotonic_ns"] + a["duration_ns"] >= b["monotonic_ns"]
                             for a, b in zip(dispatches, dispatches[1:])):
        raise ValueError("missing or ambiguous overlapping frame dispatches")
    swap_counts = {at: 0 for at in starts}
    for row in marks:
        if row["name"] != "Meta::StageImpl::swap_framebuffer()":
            continue
        index = bisect.bisect_right(starts, row["monotonic_ns"]) - 1
        if index < 0:
            raise ValueError("swap without a dispatch owner")
        owner = dispatches[index]
        if row["monotonic_ns"] + row["duration_ns"] > owner["monotonic_ns"] + owner["duration_ns"]:
            raise ValueError("swap outside its dispatch owner")
        swap_counts[owner["monotonic_ns"]] += 1
    pending = []
    presented = []
    aborted = 0
    for row in events:
        kind = kinds[row["name"]]
        if kind == "dispatch":
            if len(pending) == 2:
                raise ValueError("presentation slots overflow; incomplete ownership trace")
            pending.append(row)
            continue
        if not pending:
            raise ValueError("frame completion without a dispatch owner")
        if kind == "ready":
            if row["message"] != output:
                raise ValueError("unexpected output in frame abort")
            owner = pending.pop()  # notify_ready clears next_next, else next.
            if not (owner["monotonic_ns"] <= row["monotonic_ns"] and
                    row["monotonic_ns"] + row["duration_ns"] <= owner["monotonic_ns"] + owner["duration_ns"]):
                raise ValueError("abort is outside its newest dispatch owner")
            if swap_counts[owner["monotonic_ns"]] != 0:
                raise ValueError("aborted frame also swapped")
            aborted += 1
            continue
        owner = pending.pop(0)  # notify_presented steals next, then advances next_next.
        if swap_counts[owner["monotonic_ns"]] != 1:
            raise ValueError("presented frame lacks one unambiguous swap")
        match = re.fullmatch(re.escape(output) +
                             r", presentation (was|will be) (\d+) µs (earlier|later), KMS update ready at (\d+) µs",
                             row["message"])
        if not match or (match[1], match[3]) not in (("was", "earlier"), ("will be", "later")):
            raise ValueError("unknown kernel presentation description")
        delta = int(match[2]) * 1000 * (1 if match[1] == "will be" else -1)
        # g_get_monotonic_time runs inside the scope; its µs quantization and
        # the capture boundary each need explicit rounding room.
        lower = row["monotonic_ns"] + delta - 1000
        upper = row["monotonic_ns"] + row["duration_ns"] + delta + 1000
        if lower < owner["monotonic_ns"] or row["monotonic_ns"] < owner["monotonic_ns"] + owner["duration_ns"]:
            raise FrameOwnershipError("presentation precedes its owned frame", {
                "pid": pid, "output": output,
                "dispatch": owner, "notification": row,
                "presentation_lower_ns": lower, "presentation_upper_ns": upper,
                "kms_ready_ns": int(match[4]) * 1000,
                "swap_count": swap_counts[owner["monotonic_ns"]],
                "remaining_pending_dispatches": pending,
                "limitation": "Dispatch ownership reconstructed from scope order; no independent source frame identifier or raw kernel flip event."})
        kms_ready = int(match[4]) * 1000
        # KMS feedback readiness is a userspace timestamp, not the kernel flip
        # timestamp. It can follow that flip; it must precede this notification.
        completion_end = row["monotonic_ns"] + row["duration_ns"]
        if not owner["monotonic_ns"] <= kms_ready <= completion_end:
            raise ValueError("KMS readiness is outside its owned frame lifetime")
        presented.append(dict(dispatch_ns=owner["monotonic_ns"],
                              presentation_lower_ns=lower, presentation_upper_ns=upper,
                              kms_ready_ns=kms_ready))
    if pending:
        raise ValueError("incomplete profile leaves pending presentation slots")
    inputs = sorted((row for row in marks if row["name"] == "Meta::Display::handle_event()"
                     and row["message"] == "key-release"), key=lambda row: row["monotonic_ns"])
    if len(inputs) != expected_inputs:
        raise ValueError("isolated Super workload has an unexpected release count")
    rows = []
    seen_dispatches = set()
    for index, row in enumerate(inputs):
        start = row["monotonic_ns"]
        end = start + row["duration_ns"]
        if any(owner["monotonic_ns"] <= end and owner["monotonic_ns"] + owner["duration_ns"] >= start
               for owner in dispatches):
            raise ValueError("input overlaps frame dispatch; action association is ambiguous")
        frame = next((frame for frame in presented if frame["dispatch_ns"] > end), None)
        if frame is None or frame["dispatch_ns"] in seen_dispatches:
            raise ValueError("input has no distinct later presented frame")
        if index + 1 < len(inputs) and frame["presentation_upper_ns"] >= inputs[index + 1]["monotonic_ns"]:
            raise ValueError("response overlaps another input")
        seen_dispatches.add(frame["dispatch_ns"])
        rows.append(dict(index=index, input_start_ns=start, input_end_ns=end, **frame,
                         latency_lower_ms=(frame["presentation_lower_ns"] - end) / 1e6,
                         latency_upper_ms=(frame["presentation_upper_ns"] - start) / 1e6))
    return dict(inputs=rows, dispatch_count=len(dispatches), presented_count=len(presented),
                aborted_count=aborted, pending_count=0,
                limitation="Controlled Super-release handler bounds to first later owned frame; not device arrival or an exact accepted-toggle timestamp.")


def frame_source_evidence(decoded, pid):
    """Validate recorded source identifiers without replacing timing ownership."""
    import re
    formats = {
        "Roost::FrameClock::dispatch-id": ("dispatches",
            r"output=(\S+) frame=(\d+) dispatch_us=(\d+)"),
        "Roost::FrameClock::presented-id": ("presentations",
            r"output=(\S+) view_frame=(\d+) global_frame=(\d+) presentation_us=(\d+) sequence=(\d+) flags=(\d+) kms_ready_us=(\d+)"),
        "Roost::KMS::raw-page-flip": ("kernel_events",
            r"crtc=(\d+) sequence=(\d+) seconds=(\d+) microseconds=(\d+) device=(/dev/dri/card\d+)"),
    }
    result = {kind: [] for kind, _ in formats.values()}
    for row in decoded["marks"]:
        if row["pid"] != pid or row["name"] not in formats:
            continue
        kind, pattern = formats[row["name"]]
        match = re.fullmatch(pattern, row["message"])
        if not match:
            raise ValueError("malformed diagnostic source frame mark")
        values = match.groups()
        if kind == "kernel_events":
            crtc, sequence, seconds, microseconds = map(int, values[:4])
            if microseconds >= 1000000 or any(v > 0xffffffff for v in (crtc, sequence, seconds)):
                raise ValueError("invalid diagnostic raw kernel event")
            item = dict(crtc=crtc, sequence=sequence, seconds=seconds,
                        microseconds=microseconds, device=values[4])
        elif kind == "presentations":
            if values[0] != "Virtual-1" or int(values[4]) > 0xffffffff or int(values[5]) > 0xffffffff:
                raise ValueError("invalid diagnostic presentation identity")
            item = dict(output=values[0], frame_counter=int(values[1]),
                        global_frame_counter=int(values[2]), source_time_us=int(values[3]),
                        sequence=int(values[4]), flags=int(values[5]), kms_ready_us=int(values[6]))
        else:
            if values[0] != "Virtual-1":
                raise ValueError("unexpected diagnostic output")
            item = dict(output=values[0], frame_counter=int(values[1]), source_time_us=int(values[2]))
        result[kind].append(dict(mark_monotonic_ns=row["monotonic_ns"], **item))
    for kind, scope in (("dispatches", "Clutter::FrameClock::dispatch()"),
                        ("presentations", "Clutter::FrameClock::presented()")):
        expected = sum(row["pid"] == pid and row["name"] == scope for row in decoded["marks"])
        counters = [row["frame_counter"] for row in result[kind]]
        if not expected or len(counters) != expected or len(set(counters)) != expected:
            raise ValueError("missing or duplicate diagnostic source frame identifiers")
    if not result["kernel_events"]:
        raise ValueError("missing diagnostic raw kernel events")
    result["limitation"] = ("Instrumented Mutter 51.0-1.2 diagnostic capture; source counters and raw kernel event fields are retained independently. "
                            "No inferred frame-to-kernel pairing, timestamp substitution or final performance parity claim.")
    return result
