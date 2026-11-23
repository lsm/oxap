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
if mode == "provider":
    assert os.environ["ANTHROPIC_BASE_URL"] == "http://127.0.0.1:1/gateway"
    assert os.environ["ANTHROPIC_API_KEY"] == "oap-unauthenticated"
    assert "ANTHROPIC_AUTH_TOKEN" not in os.environ
    assert "CLAUDE_CODE_OAUTH_TOKEN" not in os.environ
    assert os.environ["ANTHROPIC_CUSTOM_HEADERS"] == "X-Host: first\nX-Trace: two words\nx-trace: third"
arguments = sys.argv[1:]
binding = arguments[arguments.index("--session-id") + 1]
assert uuid.UUID(binding).version == 4
Path("binding").write_text(binding)
frames = [json.loads(line) for line in Path(os.environ["FIXTURE"]).read_text().splitlines()]


def exchange(expected, reply, refuse=False, observation=None, close_stdin=False):
    line = sys.stdin.readline()
    actual = json.loads(line)
    wire = {"request": dict(sorted(expected["request"].items())),
            "request_id": actual["request_id"], "type": expected["type"]}
    assert line == json.dumps(wire, separators=(",", ":")) + "\n", line
    if close_stdin:
        os.close(0)
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
         None if mode == "catalog" or mode.startswith(("admission-", "run-")) else init, close_stdin=mode == "submit-write-refused")
if mode == "submit-write-refused":
    deadline = time.monotonic() + 5
    while not Path("release-turn").exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    sys.exit(0)
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
if mode.startswith("run-"):
    def send(raw, corpus_uuid, native_uuid):
        raw = dict(raw, session_id=binding)
        if raw.get("user_message_uuid") == corpus_uuid:
            raw["user_message_uuid"] = native_uuid
        if "user_message_uuids" in raw:
            raw["user_message_uuids"] = [
                native_uuid if marker == corpus_uuid else marker
                for marker in raw["user_message_uuids"]
            ]
        print(json.dumps(raw), flush=True)

    def wait_for_host(index):
        deadline = time.monotonic() + 5
        while not Path(f"continue-{index}").exists():
            assert time.monotonic() < deadline, "caller never received admission"
            time.sleep(0.01)

    disconnect_cases = {"run-process-exit", "run-malformed",
                        "run-native-wins", "run-native-buffered", "run-aborted"}
    run_index = 0
    for entry in frames[2:]:
        action, raw = entry["action"], entry["raw"]
        if action == "submit":
            assert select.select([sys.stdin], [], [], 5)[0], "missing user turn"
            turn = json.loads(sys.stdin.readline())
            corpus_uuid, native_uuid = raw["uuid"], turn["uuid"]
            assert turn == dict(raw, uuid=native_uuid)
            buffered = []
        elif action == "observe":
            if mode == "run-buffered" and raw.get("event", {}).get("type") == "message_start":
                buffered.append(raw)
                continue
            if raw["type"] == "result":
                for echo in buffered:
                    send(echo, corpus_uuid, native_uuid)
                if mode != "run-native-buffered":
                    wait_for_host(run_index)
                send(dict(raw, user_message_uuid="", user_message_uuids=[]), corpus_uuid, native_uuid)
                if mode == "run-native-wins":
                    raw = dict(raw, structured_output={"findings": [{"line": 3}]})
                if mode == "run-native-buffered":
                    raw = dict(raw, structured_output=None)
                if mode == "run-aborted":
                    raw = dict(raw, subtype="error_max_turns", terminal_reason="aborted_tools",
                               errors=["fixture abort"])
            send(raw, corpus_uuid, native_uuid)
            if (mode == "run-streaming" or mode in disconnect_cases - {"run-native-buffered"}) and raw.get("event", {}).get("type") == "message_start":
                wait_for_host(run_index)
            if raw["type"] == "result":
                send(raw, corpus_uuid, native_uuid)
                run_index += 1
        elif action == "decode-error":
            print(raw, flush=True)
            break
        elif action == "process-exit":
            break
    if mode in disconnect_cases:
        sys.exit(1)
    if select.select([sys.stdin], [], [], 5)[0]:
        assert not sys.stdin.readline(), "extra native input"
        Path("eof").touch()
    sys.exit(0)

if mode in {"submit-text", "submit-parts", "submit-abandon"} or mode.startswith("admission-"):
    raw = sys.stdin.readline()
    frame = json.loads(raw)
    assert frame["uuid"].startswith("turn-")
    assert uuid.UUID(frame["uuid"].removeprefix("turn-")).version == 4
    expected = frames[2]["raw"]
    expected["uuid"] = frame["uuid"]
    if mode == "submit-parts":
        del expected["client_composed"]
        expected["message"]["content"] = "你好 <&>\nsecond line"
    assert frame == expected, frame
    wire = json.dumps(expected, separators=(",", ":"), sort_keys=True, ensure_ascii=False)
    wire = wire.replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    assert raw == wire + "\n", raw
    if mode.startswith("admission-"):
        def observe(raw):
            raw["session_id"] = binding
            print(json.dumps(raw), flush=True)

        def barrier():
            print(json.dumps({"type": "control_request", "request_id": "barrier",
                              "request": {"subtype": "barrier"}}), flush=True)
            assert json.loads(sys.stdin.readline())["response"]["request_id"] == "barrier"

        observe(dict(init, model="earlier-buffered-model"))
        for observed in frames[3:6]:
            native = observed["raw"]
            if "command_uuid" in native:
                native["command_uuid"] = frame["uuid"]
            observe(native)

        echo = frames[6]["raw"]
        observe(dict(init, model="foreign-model", user_message_uuid="foreign"))
        observe(dict(echo, user_message_uuid="foreign", user_message_uuids=["foreign"]))
        observe(dict(echo, user_message_uuid=frame["uuid"], user_message_uuids=[], parent_tool_use_id="child"))
        observe(dict(frames[8]["raw"], user_message_uuid=frame["uuid"], user_message_uuids=[],
                     origin={"kind": "task-notification"}))
        barrier()

    Path("turn-observed").touch()
    if mode == "submit-abandon":
        assert not sys.stdin.readline(), "extra native input"
        Path("eof").touch()
        sys.exit(0)
    deadline = time.monotonic() + 10
    while not Path("release-turn").exists():
        assert not select.select([sys.stdin], [], [], 0.02)[0], "extra native input"
        assert time.monotonic() < deadline, "submit did not release peer"
    assert not select.select([sys.stdin], [], [], 0)[0], "extra native input"
    if mode.startswith("admission-"):
        if mode == "admission-compacting":
            observe({"type": "system", "subtype": "status", "status": "compacting"})
        else:
            single = mode != "admission-list"
            observe(dict(echo, user_message_uuid=frame["uuid"] if single else "",
                         user_message_uuids=[] if single else [frame["uuid"]]))
        if mode == "admission-disconnect":
            barrier()
            Path("echo-drained").touch()
            sys.exit(0)
        ready = select.select([sys.stdin], [], [], 3)[0]
        if ready:
            assert not sys.stdin.readline(), "extra native input"

    Path("no-extra-turn").touch()
    sys.exit(0)
assert not sys.stdin.readline()
Path("eof").touch()
