#!/usr/bin/python3
"""Original Wayland requests and independent X11-host pixels, without audio claims."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

from PIL import ImageGrab


def run(*args):
    return subprocess.check_output(args, text=True, timeout=10).strip()


def process_identity(pid):
    root = Path('/proc') / str(pid)
    fields = (root / 'stat').read_text().rsplit(')', 1)[1].split()
    return {'pid': pid, 'uid': root.stat().st_uid, 'start_ticks': int(fields[19]),
            'exe': str((root / 'exe').resolve()),
            'argv_hex': (root / 'cmdline').read_bytes().hex()}


def wait_for(description, predicate):
    end = time.monotonic() + 15
    while time.monotonic() < end:
        result = predicate()
        if result:
            return result
        time.sleep(.02)
    raise RuntimeError('timed out: ' + description)


def median_rgb(image, point):
    x, y = point
    values = list(image.crop((x-2, y-2, x+3, y+3)).convert('RGB').getdata())
    return [int(statistics.median(p[c] for p in values)) for c in range(3)]


def delta(a, b):
    return max(abs(x-y) for x, y in zip(a, b))


def main():
    parser = argparse.ArgumentParser()
    for name in ('root', 'state', 'socket', 'host', 'compositor', 'out'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    executable = Path(args.root) / 'target/debug/examples/system-bell-client'
    host_owner = int(run('xdotool', 'getwindowpid', args.host))
    if host_owner != int(args.compositor):
        raise RuntimeError('host window does not belong to the original compositor')
    compositor = process_identity(host_owner)
    geometry = dict(line.split('=', 1) for line in
                    run('xdotool', 'getwindowgeometry', '--shell', args.host).splitlines())
    x, y, width, height = (int(geometry[k]) for k in ('X', 'Y', 'WIDTH', 'HEIGHT'))
    # This lane is explicitly scale 1; the independent scale matrix remains
    # mandatory and does not count these observations as scaled bell proof.
    if (width, height) != (1280, 800):
        raise RuntimeError('unexpected host geometry for bell pixel lane')
    control = out / 'requests.txt'
    log = out / 'principal.log'
    app_id = 'org.roost.SystemBellProof'
    schema = 'org.gnome.desktop.wm.preferences'
    settings = [(schema, k) for k in ('audible-bell', 'visual-bell', 'visual-bell-type')]
    settings.append(('org.gnome.desktop.sound', 'event-sounds'))
    original = {(s, k): run('gsettings', 'get', s, k) for s, k in settings}
    environment = dict(os.environ, WAYLAND_DISPLAY=args.socket)
    for key in ('DISPLAY', 'WAYLAND_SOCKET'):
        environment.pop(key, None)
    report = {'host': geometry, 'compositor': compositor, 'scale': 1,
              'limitations': ['nested X11-host presentation only', 'no audio evidence',
                              'no native or multiple-output qualification'],
              'client_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'original_settings': {s + '/' + k: v for (s, k), v in original.items()},
              'cases': []}
    child = None

    def state():
        return json.loads(Path(args.state).read_text())

    def grab():
        start = time.monotonic_ns()
        image = ImageGrab.grab(bbox=(x, y, x+width, y+height),
                               xdisplay=os.environ['DISPLAY']).convert('RGB')
        return start, time.monotonic_ns(), image

    try:
        with log.open('w') as stream:
            child = subprocess.Popen([str(executable), app_id, str(control)],
                                     env=environment, stdout=stream, stderr=stream)
        wait_for('original principal ready', lambda: 'READY pid=' in log.read_text())
        principal = process_identity(child.pid)
        if principal['uid'] != os.getuid() or principal['exe'] != str(executable.resolve()):
            raise RuntimeError('bell principal credentials or executable differ')
        report['principal'] = principal
        window = wait_for('original mapped window', lambda: next(
            (w for w in state()['windows'] if w['app_id'] == app_id), None))
        if state()['overview_open'] or state()['locked']:
            raise RuntimeError('bell lane requires an unlocked ordinary desktop')
        rect = window['rect']
        inside = [rect[0]+rect[2]//2, rect[1]+rect[3]//2]
        wait_for('genuine RGB240 window pixels', lambda: delta(grab()[2].getpixel(
            tuple(inside)), [240, 240, 240]) <= 1)
        sequence = 0
        for audible in (False, True):
            for visual in (False, True):
                for fullscreen, target in ((False, 'window'), (True, 'window'),
                                           (False, 'whole')):
                    sequence += 1
                    case = {'sequence': sequence, 'audible': audible, 'visual': visual,
                            'fullscreen': fullscreen, 'target': target, 'samples': []}
                    report['cases'].append(case)
                    run('gsettings', 'set', schema, 'audible-bell', str(audible).lower())
                    run('gsettings', 'set', schema, 'visual-bell', str(visual).lower())
                    run('gsettings', 'set', schema, 'visual-bell-type',
                        'fullscreen-flash' if fullscreen else 'frame-flash')
                    # Isolate visual policy. Positive sound/PCM is a separate
                    # real PipeWire acceptance requirement, never a stub here.
                    run('gsettings', 'set', 'org.gnome.desktop.sound', 'event-sounds', 'false')
                    expected = {'audible': audible, 'visual': visual,
                                'fullscreen': fullscreen, 'event_sounds': False}
                    def applied_policy():
                        policy = state().get('bell_policy')
                        return isinstance(policy, dict) and all(
                            policy.get(k) == v for k, v in expected.items())

                    wait_for('applied live compositor policy', applied_policy)
                    before = state()['bell_requests']
                    first = grab()[2]
                    baseline = median_rgb(first, inside)
                    if delta(baseline, [240, 240, 240]) > 1:
                        raise RuntimeError('original fixed client is not visibly RGB240')
                    # Choose the outside probe before the bell, away from the
                    # original window and panel, with sufficient visible RGB.
                    candidates = [(px, py) for py in range(height-20, 60, -40)
                                  for px in range(20, width-20, 40)
                                  if not (rect[0]-5 <= px <= rect[0]+rect[2]+5 and
                                          rect[1]-5 <= py <= rect[1]+rect[3]+5)]
                    outside = next((p for p in candidates if max(median_rgb(first, p)) >= 40), None)
                    if outside is None:
                        raise RuntimeError('no measurable point outside the requested window')
                    outside_base = median_rgb(first, outside)
                    case.update(inside=inside, outside=outside, baseline=baseline,
                                outside_baseline=outside_base, policy=state()['bell_policy'])
                    first.save(out / f'{sequence:02}-before.png')
                    begun = time.monotonic_ns()
                    sent = False
                    darkest = (240, first)
                    while (time.monotonic_ns()-begun) / 1e9 < 1:
                        start, end, image = grab()
                        elapsed = (start-begun) / 1e9
                        if not sent and elapsed >= .25:
                            request = control.with_suffix('.tmp')
                            request.write_text(f'{sequence} {target}\n')
                            # Record the lower bound before atomic visibility;
                            # the original client may consume immediately.
                            case['request_written_ns'] = time.monotonic_ns()
                            request.replace(control)
                            sent = True
                        sample = {'start_ns': start, 'end_ns': end,
                                  'inside': median_rgb(image, inside),
                                  'outside': median_rgb(image, outside)}
                        case['samples'].append(sample)
                        if max(sample['inside']) < darkest[0]:
                            darkest = (max(sample['inside']), image)
                        time.sleep(.01)
                    image.save(out / f'{sequence:02}-after.png')
                    darkest[1].save(out / f'{sequence:02}-darkest.png')
                    rows = case['samples']
                    intervals = [(b['end_ns']-a['start_ns'])/1e6 for a, b in zip(rows, rows[1:])]
                    case['maximum_observation_interval_ms'] = max(intervals)
                    if max(intervals) > 50:
                        raise RuntimeError('host observations cannot resolve the 100ms visual pattern')
                    wait_for('actual original wire request', lambda:
                             f'RING seq={sequence} target={target} ' in log.read_text())
                    line = next(line for line in log.read_text().splitlines()
                                if line.startswith(f'RING seq={sequence} target={target} '))
                    wire = dict(field.split('=', 1) for field in line.split()[1:])
                    case['wire_before_ns'] = int(wire['before_ns'])
                    case['wire_after_ns'] = int(wire['after_ns'])
                    if not (case['request_written_ns'] <= case['wire_before_ns'] <=
                            case['wire_after_ns']) or (case['wire_after_ns']-
                                                      case['wire_before_ns']) > 50_000_000:
                        raise RuntimeError('actual wire admission cannot resolve the flash interval')
                    if state()['bell_requests'] != before+1:
                        raise RuntimeError('not exactly one actual bell admitted')
                    post = [r for r in rows if r['start_ns'] >= case['request_written_ns']]
                    flashes = [r for r in post if max(r['inside']) < 220 and
                               max(r['inside'])-min(r['inside']) <= 2]
                    case['observed_flash_samples'] = len(flashes)
                    if bool(flashes) != visual:
                        raise RuntimeError('actual presented flash disagrees with visual policy')
                    # Mutter's longest pattern is 150ms. Allow one bounded
                    # host observation interval and one 60Hz presentation
                    # interval for the expiry to become independently visible.
                    expiry = case['wire_after_ns'] + 150_000_000 + int(max(intervals)*1e6) + 16_666_667
                    if any(delta(r['inside'], baseline) > 2 or
                           delta(r['outside'], outside_base) > 2 for r in post
                           if r['start_ns'] > expiry):
                        raise RuntimeError('actual presented flash exceeded the GNOME expiry bound')
                    stage = visual and (fullscreen or target == 'whole')
                    outside_flashes = [r for r in post if
                                       max(r['outside']) < max(outside_base)-5]
                    if bool(outside_flashes) != stage:
                        raise RuntimeError('outside-window pixels disagree with the requested flash scope')
                    if not stage and any(delta(r['outside'], outside_base) > 2 for r in rows):
                        raise RuntimeError('frame-only/disabled alert changed outside pixels')
                    if not visual and any(delta(r['inside'], baseline) > 2 for r in rows):
                        raise RuntimeError('disabled visual alert changed original client pixels')
                    if delta(rows[-1]['inside'], baseline) > 1 or delta(rows[-1]['outside'], outside_base) > 2:
                        raise RuntimeError('bell pixels did not return to baseline after expiry')
                    if process_identity(child.pid) != principal or process_identity(host_owner) != compositor:
                        raise RuntimeError('original principal or compositor lifetime changed')
                    case['pass'] = True
        report['pass'] = True
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        cleanup_errors = []
        for (s, k), value in original.items():
            try:
                run('gsettings', 'set', s, k, value)
            except Exception as error:
                cleanup_errors.append(str(error))
        if child is not None:
            try:
                child.terminate()
                child.wait(timeout=5)
            except Exception as error:
                cleanup_errors.append(str(error))
                try:
                    child.kill()
                    child.wait(timeout=5)
                except Exception as final_error:
                    cleanup_errors.append(str(final_error))
        report['cleanup_errors'] = cleanup_errors
        (out / 'report.json').write_text(json.dumps(report, indent=2))
        if cleanup_errors and 'error' not in report:
            raise RuntimeError('bell proof cleanup failed')


if __name__ == '__main__':
    main()
