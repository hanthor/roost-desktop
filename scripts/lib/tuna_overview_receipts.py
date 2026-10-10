"""Bounded Tuna card-complete receipts; first-response pixels cannot qualify."""
import base64
import hashlib
import json
import re

MAX_BYTES = 192 * 1024


def integer(value, name, minimum=0):
    if type(value) is not int or not minimum <= value <= 2**64 - 1:
        raise ValueError('invalid ' + name)
    return value


def process_identity(value):
    if not isinstance(value, dict):
        raise ValueError('missing compositor identity')
    for field in ('pid', 'start_ticks', 'uid'):
        integer(value.get(field), field, 1 if field != 'uid' else 0)
    for field, pattern in (('boot_id', r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}'), ('sha256', r'[0-9a-f]{64}')):
        if not isinstance(value.get(field), str) or not re.fullmatch(pattern, value[field]):
            raise ValueError('invalid compositor ' + field)
    if value.get('path') != '/usr/bin/tuna-compositor' or value.get('trace_enabled') is not True:
        raise ValueError('unqualified compositor executable/environment')
    return value


def journal_rows(raw, identity, cursor=None):
    """Only journald's trusted PID/UID/executable/boot fields bind a message."""
    process_identity(identity)
    if not isinstance(raw, bytes) or len(raw) > MAX_BYTES:
        raise ValueError('journal acquisition bound')
    rows = []
    last = cursor
    for line in raw.splitlines():
        row = json.loads(line)
        if not isinstance(row, dict):
            raise ValueError('invalid journal row')
        message = row.get('MESSAGE', '')
        if not isinstance(message, str) or not message.startswith('tuna-perf-input: '):
            continue
        if (row.get('_PID') != str(identity['pid']) or row.get('_UID') != str(identity['uid'])
                or row.get('_EXE') != identity['path']
                or row.get('_BOOT_ID') != identity['boot_id'].replace('-', '')):
            raise ValueError('journal process identity differs')
        last = row.get('__CURSOR')
        if not isinstance(last, str) or not 1 <= len(last) <= 512:
            raise ValueError('missing journal cursor')
        event = json.loads(message.removeprefix('tuna-perf-input: '))
        if not isinstance(event, dict) or event.get('kind') not in ('input', 'card-candidate', 'queued', 'presented', 'discarded'):
            raise ValueError('invalid native event')
        integer(event.get('id'), 'input ID', 1)
        if len(rows) >= 256:
            raise ValueError('native receipt count bound')
        rows.append(event)
    return rows, last


def counts(row):
    if not isinstance(row, dict):
        raise ValueError('missing card counts')
    expected = row.get('expected')
    if expected is not None:
        integer(expected, 'expected cards')
    for key in ('rendered', 'cached', 'pending'):
        integer(row.get(key), key)
    return expected is not None and expected == row['rendered']


def validate_capture(receipt, expected_process, expected_image, binary_sha256, source_sha, fixture_image_id):
    """Require original workload's 20 actions and actual, ordered primary flips."""
    identity = process_identity(receipt.get('process'))
    if identity != process_identity(expected_process) or identity['sha256'] != binary_sha256:
        raise ValueError('compositor identity changed or original payload differs')
    if receipt.get('reference_image') != expected_image:
        raise ValueError('reference image differs')
    if not re.fullmatch(r'[0-9a-f]{40}', source_sha or '') or not re.fullmatch(r'sha256:[0-9a-f]{64}', fixture_image_id or ''):
        raise ValueError('missing original source/image attribution')
    if not isinstance(receipt.get('cursor'), str) or not 1 <= len(receipt['cursor']) <= 512:
        raise ValueError('missing original workload cursor')
    if not isinstance(receipt.get('boundary_journal_sha256'), str) or not re.fullmatch(r'[0-9a-f]{64}', receipt['boundary_journal_sha256']):
        raise ValueError('missing original boundary journal identity')
    source = receipt.get('journal_source')
    if not isinstance(source, dict) or not isinstance(source.get('data'), str) or len(source['data']) > ((MAX_BYTES+2)//3)*4:
        raise ValueError('missing bounded original journal bytes')
    raw = base64.b64decode(source['data'], validate=True)
    if len(raw) != source.get('bytes') or hashlib.sha256(raw).hexdigest() != source.get('sha256'):
        raise ValueError('journal source digest differs')
    replayed, _ = journal_rows(raw, identity, receipt.get('cursor'))
    events = receipt.get('events')
    if replayed != events:
        raise ValueError('native events differ from original journal')
    if not isinstance(events, list) or len(events) > 256:
        raise ValueError('missing bounded native events')
    inputs, candidates, queued, presented = {}, {}, {}, {}
    after_id = integer(receipt.get('after_id'), 'workload boundary ID')
    for event in events:
        if not isinstance(event, dict):
            raise ValueError('invalid native event')
        ident = integer(event.get('id'), 'input ID', 1)
        if ident <= after_id:
            raise ValueError('pre-workload event included')
        kind = event.get('kind')
        target = {'input': inputs, 'card-candidate': candidates, 'queued': queued, 'presented': presented}.get(kind)
        if target is None or ident in target:
            raise ValueError('discarded, duplicate or unknown native event')
        target[ident] = event
    ids = list(range(after_id + 1, after_id + 21))
    if any(set(bucket) != set(ids) for bucket in (inputs, candidates, queued, presented)):
        raise ValueError('missing original twenty overview actions')
    samples = []
    previous_sequence = -1
    for index, ident in enumerate(ids):
        action, candidate, queue, flip = (bucket[ident] for bucket in (inputs, candidates, queued, presented))
        if any(type(row.get('schema')) is not int or row['schema'] != 2 for row in (action, candidate, flip)):
            raise ValueError('unsupported readiness schema')
        if action.get('opening') is not (index % 2 == 0):
            raise ValueError('overview workload direction differs')
        start = integer(action.get('input_ns'), 'input timestamp')
        queued_ns = integer(queue.get('queued_ns'), 'queue timestamp')
        end = integer(flip.get('presented_ns'), 'presentation timestamp')
        candidate_ns = integer(candidate.get('candidate_ns'), 'candidate timestamp')
        if not start <= candidate_ns <= queued_ns <= end or end - start > 10_000_000_000:
            raise ValueError('unordered or excessive native latency')
        if integer(flip.get('input_ns'), 'associated input timestamp') != start or integer(flip.get('queued_ns'), 'associated queue timestamp') != queued_ns or flip.get('cards_complete') is not True:
            raise ValueError('presentation association differs')
        sequence = integer(flip.get('sequence'), 'kernel sequence')
        if sequence <= previous_sequence:
            raise ValueError('primary presentation sequence does not advance')
        previous_sequence = sequence
        if flip.get('first_cards') != candidate.get('cards') or not counts(flip.get('presented_cards')):
            raise ValueError('missing current-layout card completion')
        first_complete = counts(candidate.get('cards'))
        if candidate.get('preparation') != ('ready-on-first-candidate' if first_complete else 'incomplete-on-first-candidate'):
            raise ValueError('candidate classification differs')
        if index % 2 == 0 and flip['presented_cards']['expected'] == 0:
            raise ValueError('desktop frame cannot qualify overview opening')
        samples.append({'index': index, 'action': 'overview-open' if index % 2 == 0 else 'overview-close',
                        'input_to_card_complete_scanout_s': (end-start)/1e9,
                        'id': ident, 'sequence': sequence, 'first_cards': flip['first_cards'],
                        'presented_cards': flip['presented_cards'], 'preparation': candidate['preparation']})
    raw = json.dumps(receipt, sort_keys=True).encode()
    return {'status': 'acquired', 'qualifies_completed_overview': True, 'samples': samples,
            'scope': 'current-layout wallpaper cards at associated native primary scanout; not animation or GTK completion',
            'process': identity, 'reference_image': expected_image, 'source_sha': source_sha,
            'source_attribution': 'original successful MAIN package artifact; not source reproducibility attestation',
            'fixture_image_id': fixture_image_id, 'image_attribution': 'host image installed into owned disk; reference image checked in guest',
            'receipt_sha256': hashlib.sha256(raw).hexdigest(),
            'pre_input_preparation': 'unobserved', 'startup_accounting': 'unchanged whole-run CPU/RSS/PSS samples include preparation'}
