#!/usr/bin/env python3
"""PipeWire JSON event fixture; mutations wake a FIFO, never a polling loop."""
import json
import os
from pathlib import Path
import sys

root = Path(sys.argv[1])
fifo = root / 'audio-events'
if '--event' in sys.argv:
    fd = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
    try:
        os.write(fd, b'changed\n')
    finally:
        os.close(fd)
    sys.exit(0)


def number(name, fallback):
    try:
        return float((root / name).read_text())
    except (OSError, ValueError):
        return fallback


def node(ident, name, cls, volume, **props):
    return {'id': ident, 'type': 'PipeWire:Interface:Node', 'info': {
        'state': 'running', 'props': {'node.name': name, 'node.description': name,
                                    'media.class': cls, **props},
        'params': {'Props': [{'mute': False, 'channelVolumes': [volume ** 3]}]}}}


def emit():
    selected = int(number('default-sink', 48))
    route = int(number('audio-route', 1))
    batch = [node(48, 'Built-in Audio Analog Stereo', 'Audio/Sink', number('volume', .8),
                  **{'device.id': 40, 'card.profile.device': 0}),
             node(52, 'Roost HDMI Output', 'Audio/Sink', 1),
             node(49, 'Microphone', 'Audio/Source', number('mic-volume', .5)),
             {'id': 40, 'type': 'PipeWire:Interface:Device', 'info': {'params': {
                 'EnumRoute': [{'index': 1, 'direction': 'Output', 'description': 'Speakers',
                                'available': 'yes', 'devices': [0]},
                               {'index': 2, 'direction': 'Output', 'description': 'Headphones',
                                'available': 'unknown', 'devices': [0]}],
                 'Route': [{'index': route, 'device': 0}]}}},
             {'id': 20, 'type': 'PipeWire:Interface:Metadata',
              'props': {'metadata.name': 'default'}, 'metadata': [
                  {'subject': 0, 'key': 'default.audio.sink', 'value': {
                      'name': 'Roost HDMI Output' if selected == 52 else 'Built-in Audio Analog Stereo'}},
                  {'subject': 0, 'key': 'default.audio.source', 'value': {'name': 'Microphone'}}]}]
    if (root / 'recording').exists():
        batch.append(node(70, 'Voice Recorder', 'Stream/Input/Audio', 1))
    else:
        batch.append({'id': 70, 'info': None})
    print(json.dumps(batch), flush=True)


try:
    os.mkfifo(fifo, 0o600)
except FileExistsError:
    pass
# RDWR keeps the reader alive between independently opened event writers.
fd = os.open(fifo, os.O_RDWR)
with (root / "audio-monitor-pids").open("a") as pids:
    pids.write(f"{os.getpid()} {os.getppid()}\n")
emit()
with os.fdopen(fd, 'rb', buffering=0) as events:
    while events.read(4096):
        emit()
