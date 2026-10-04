"""Incrementally validate actual native presentation feedback during a soak."""
import json
import math
import statistics


def distribution(values):
    values = sorted(values)
    if not values:
        return {"count": 0}
    def quantile(p):
        return values[max(0, math.ceil(len(values) * p) - 1)]
    return dict(count=len(values), p50=quantile(.5), p95=quantile(.95),
                p99=quantile(.99), min=values[0], max=values[-1], mean=statistics.mean(values))


def integer(row, key, maximum=0xffffffffffffffff):
    value = row.get(key)
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError(f"invalid presentation {key}")
    return value


class Reader:
    def __init__(self, stream, seconds):
        self.stream = stream
        self.seconds = seconds
        self.batch = self.presented = self.commits = self.discarded = 0
        self.clock = self.previous_ns = self.previous_commit = None
        self.complete = False
        self.elapsed = 0
        self.partial = False

    def poll(self):
        intervals = []
        flags = dict(vsync=0, hw_clock=0, hw_completion=0, zero_copy=0)
        unknown_refresh = frames = 0
        while True:
            position = self.stream.tell()
            line = self.stream.readline(128 * 1024 + 1)
            if len(line) > 128 * 1024:
                raise ValueError("presentation row exceeded the bounded record size")
            if not line.endswith(b"\n"):
                self.stream.seek(position)  # A producer may be midway through a row.
                break
            if self.complete:
                raise ValueError("presentation data follows completion")
            row = json.loads(line)
            elapsed = row.get("elapsed_s")
            if type(elapsed) not in (int, float) or not math.isfinite(elapsed) or elapsed < self.elapsed:
                raise ValueError("invalid presentation elapsed time")
            commits, discarded, pending = (integer(row, key) for key in ("commits", "discarded", "pending"))
            if commits < self.commits or discarded < self.discarded or pending > 16:
                raise ValueError("invalid cumulative presentation accounting")
            if row.get("kind") == "presentation-batch":
                if integer(row, "batch") != self.batch or self.partial:
                    raise ValueError("missing/repeated/out-of-order presentation batch")
                samples = row.get("frames")
                if not isinstance(samples, list) or not 1 <= len(samples) <= 240:
                    raise ValueError("invalid native presentation batch size")
                clock = integer(row, "clock_id", 0xffffffff)
                if self.clock is not None and self.clock != clock:
                    raise ValueError("presentation clock changed during soak")
                self.clock = clock
                if integer(row, "presented_total") != self.presented + len(samples):
                    raise ValueError("lost presentation feedback between batches")
                for sample in samples:
                    commit = integer(sample, "commit")
                    stamp = integer(sample, "presented_ns")
                    refresh = integer(sample, "refresh_ns", 0xffffffff)
                    bits = integer(sample, "flags", 0xffffffff)
                    integer(sample, "sequence")
                    if (not 1 <= commit <= commits or stamp == 0
                            or (self.previous_commit is not None and commit <= self.previous_commit)
                            or (self.previous_ns is not None and stamp <= self.previous_ns)):
                        raise ValueError("duplicate/backward native presentation feedback")
                    if self.previous_ns is not None:
                        intervals.append((stamp - self.previous_ns) / 1e6)
                    self.previous_ns, self.previous_commit = stamp, commit
                    unknown_refresh += refresh == 0
                    for name, bit in zip(flags, (1, 2, 4, 8)):
                        flags[name] += bool(bits & bit)
                frames += len(samples)
                self.presented += len(samples)
                self.batch += 1
                self.partial = len(samples) < 240
            elif row.get("kind") == "complete":
                if (integer(row, "requested_s") != self.seconds or elapsed < self.seconds
                        or integer(row, "presented_total") != self.presented or self.presented < 2):
                    raise ValueError("presentation completion does not cover the requested soak")
                self.complete = True
            else:
                raise ValueError("unknown endurance presentation record")
            if commits != self.presented + discarded + pending:
                raise ValueError("unbalanced native presentation accounting")
            self.commits, self.discarded, self.elapsed = commits, discarded, elapsed
        return dict(interval_ms=distribution(intervals), frames=frames,
                    total_frames=self.presented, batches=self.batch, complete=self.complete,
                    flags=flags, unknown_refresh=unknown_refresh, elapsed_s=self.elapsed)

    def finish(self):
        result = self.poll()
        if not self.complete or self.stream.read(1):
            raise ValueError("incomplete or truncated endurance presentation capture")
        return result
