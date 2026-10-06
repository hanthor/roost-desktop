#!/usr/bin/env python3
"""Check the actual showing calendar headings and ISO week labels, not settings."""
import collections
import datetime
import json
import sys


def check(path, first, show):
    nodes = json.load(open(path, encoding="utf-8"))
    weekdays = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]
    headings = sorted(
        [n for n in nodes if n.get("showing") and n.get("role") == "label"
         and n.get("name") in weekdays and n.get("bounds")],
        key=lambda n: n["bounds"][0],
    )
    assert len(headings) == 7, f"expected seven visible headings, got {headings}"
    start = weekdays.index(headings[0]["name"])
    if first != "default":
        assert start == weekdays.index(first), f"wrong first weekday: {headings}"
    assert [n["name"] for n in headings] == weekdays[start:] + weekdays[:start]
    weeks = [n["name"] for n in nodes if n.get("showing")
             and n.get("role") == "label" and n.get("name", "").startswith("Week ")]
    if show:
        today = datetime.date.today()
        month = today.replace(day=1)
        back = ((month.weekday() + 1) % 7 - start) % 7 or 7
        begin = month - datetime.timedelta(days=back)
        thursday = (4 - start) % 7
        expected = [f"Week {(begin + datetime.timedelta(days=thursday + 7 * row)).isocalendar().week:02d}"
                    for row in range(6)]
        assert collections.Counter(weeks) == collections.Counter(expected), (weeks, expected)
    else:
        assert not weeks, f"week labels remained visible: {weeks}"
    print(f"calendar {first}: first={headings[0]['name']}, weeks={weeks}")


if __name__ == "__main__":
    check(sys.argv[1], sys.argv[2], sys.argv[3] == "true")
