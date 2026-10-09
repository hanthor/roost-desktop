#!/usr/bin/python3
"""Validate paired strip endpoints and publish measured phase uncertainty."""
import json
from pathlib import Path
import sys
from PIL import Image, ImageChops

root = Path(sys.argv[1])
reference = root / 'reference'
candidate = root / 'candidate'
report = {'dimensions': [1280, 800], 'endpoints': {}, 'timing': {}}
for stage, width in [('01-strip-rest', 616), ('02-strip-preset', 826),
                     ('04-strip-scroll-settled', 616)]:
    actual = json.loads((candidate / (stage + '.json')).read_text())
    expected = json.loads((reference / (stage + '.json')).read_text())
    assert actual['session_mode'] == 'scroll', actual
    focused = next(w for w in actual['windows'] if w['id'] == actual['focused'])
    focused_ref = next(w for w in expected if w['is_focused'])
    assert focused['rect'][1:] == [48, width, 736], focused
    assert focused_ref['layout']['window_size'] == [width, 736], focused_ref
    assert abs(actual['strip_view'] - actual['strip_offset']) < 0.1, actual
    assert focused['rect'][0] >= 16 and sum(focused['rect'][::2]) <= 1264, focused
    images = [Image.open(p / (stage + '.png')).convert('RGB') for p in [reference, candidate]]
    assert all(i.size == (1280, 800) for i in images)
    # Same GTK runtime and source windows: check content independently of desktop gaps.
    x = focused['rect'][0]
    box = (x, 95, x + width, 634)
    diff = ImageChops.difference(images[0].crop(box), images[1].crop(box))
    hist = diff.convert('L').histogram()
    off = sum(hist[25:]) / (width * 539)
    assert off < 0.02, {'stage': stage, 'content_off': off}
    report['endpoints'][stage] = {'focused_rect': focused['rect'], 'niri_size': focused_ref['layout']['window_size'], 'content_off_percent': off * 100}
for name, directory in [('niri', reference), ('tuna', candidate)]:
    samples = json.loads((directory / 'view-timing.json').read_text())
    assert samples[0]['green_right'] == 422 and samples[-1]['green_right'] == 632, samples
    # Bound the sampling delay; report both ends of the interval, never relabel late frames.
    assert all(s['capture_started_ms'] - s['requested_ms'] < 100 for s in samples), samples
    settled = next((s['requested_ms'] for s in samples[1:] if abs(s['green_right'] - 632) <= 1), None)
    assert settled is not None and settled <= 800, samples
    report['timing'][name] = {'sampled_settle_ms': settled, 'samples': samples}
print(json.dumps(report, indent=2))
