"""Decode pw-dump's one-shot snapshot plus concurrent removal arrays."""
import json


def dump_objects(raw):
    if len(raw) > 8 * 1024 * 1024:
        raise ValueError("PipeWire dump exceeds eight MiB")
    text = raw.decode("utf-8")
    decoder = json.JSONDecoder()
    offset, documents, objects = 0, 0, []
    while offset < len(text):
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if offset == len(text):
            break
        document, offset = decoder.raw_decode(text, offset)
        documents += 1
        if documents > 64 or not isinstance(document, list):
            raise ValueError("invalid PipeWire document sequence")
        for item in document:
            if not isinstance(item, dict) or type(item.get("id")) is not int:
                raise ValueError("invalid PipeWire object")
            if isinstance(item.get("type"), str):
                objects.append(item)
            elif set(item) not in ({"id", "info"}, {"id", "props"}) or any(
                value is not None for key, value in item.items() if key != "id"
            ):
                raise ValueError("invalid PipeWire removal")
    # A removal array alone, empty output or a disconnected tool is not proof
    # of node absence. Require its real complete snapshot, including the Core.
    if not any(item["type"] == "PipeWire:Interface:Core" for item in objects):
        raise ValueError("PipeWire dump lacks a complete Core snapshot")
    # Keep every typed object across documents: observing a node anywhere
    # conservatively requires another bounded dump before claiming withdrawal.
    return objects
