#!/usr/bin/env python3
import json
import os
from pathlib import Path
import select
import sys
import time
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
    line = sys.stdin.readline()
    actual = json.loads(line)
    wire = {"request": dict(sorted(expected["request"].items())),
            "request_id": actual["request_id"], "type": expected["type"]}
    assert line == json.dumps(wire, separators=(",", ":")) + "\n", line
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
exchange(frames[0]["raw"], frames[1]["raw"], mode == "initialize-refusal",
         None if mode == "catalog" else init)
if mode == "catalog":
    for step in ("publish-initial", "publish-refresh"):
        deadline = time.monotonic() + 10
        while not Path(step).exists():
            if select.select([sys.stdin], [], [], 0.02)[0]:
                assert not sys.stdin.readline()
                sys.exit(0)
            assert time.monotonic() < deadline, step
        init["model"] = step
        if step == "publish-refresh":
            name = "mcp__files__nested__read"
            init["tools"] = ["", name, name]
            init["mcp_servers"] = [{"name": ""}, init["mcp_servers"][0], init["mcp_servers"][0]]
        print(json.dumps(init), flush=True)
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
