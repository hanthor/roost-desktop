"""Strict structural decoding of Sysprof's documented version-1 capture marks.

The raw capture remains authoritative. This decodes marks, not causal latency.
Layout: GNOME/sysprof 51.0 src/libsysprof-capture/sysprof-capture-types.h.
"""
import struct

MAX_CAPTURE = 8 * 1024 * 1024


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
