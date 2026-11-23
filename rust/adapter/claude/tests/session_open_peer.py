#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
import uuid

assert os.environ["NODE_ENV"] == "test"
assert Path(os.environ["HOME"]) == Path.cwd()
mode = os.environ["MODE"]
arguments = sys.argv[1:]
binding = arguments[arguments.index("--session-id") + 1]
assert uuid.UUID(binding).version == 4
Path("binding").write_text(binding)
frames = [json.loads(line) for line in Path(os.environ["FIXTURE"]).read_text().splitlines()]


def exchange(expected, reply, refuse=False, observation=None):
    actual = json.loads(sys.stdin.readline())
    expected["request_id"] = actual["request_id"]
    assert actual == expected, actual
    reply["response"]["request_id"] = actual["request_id"]
    if refuse:
        reply["response"].update(subtype="error", error="fixture refusal")
        del reply["response"]["response"]
    if observation:
        print(json.dumps(observation), flush=True)
    print(json.dumps(reply), flush=True)


init = next(frame["raw"] for frame in frames if isinstance(frame["raw"], dict)
            and frame["raw"].get("subtype") == "init")
init["session_id"] = binding
exchange(frames[0]["raw"], frames[1]["raw"], mode == "initialize-refusal", init)
if mode in {"settings", "settings-refusal"}:
    exchange(
        {"type": "control_request", "request": {
            "subtype": "apply_flag_settings", "settings": {"effortLevel": "high"},
        }},
        {"type": "control_response", "response": {"subtype": "success", "response": {}}},
        mode == "settings-refusal",
    )
assert not sys.stdin.readline()
Path("eof").touch()
