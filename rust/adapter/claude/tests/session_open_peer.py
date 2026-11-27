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
reopen = mode.startswith("reopen-")
if reopen:
    assert "--session-id" not in arguments and arguments.count("--resume") == 1
    binding = arguments[-1]
    case = json.loads(Path(os.environ["FIXTURE"]).with_name("case.json").read_text())
    assert arguments[-2:] == ["--resume", case["identity_map"]["native_session_id"]]
else:
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


if reopen:
    exchange(frames[0]["raw"], frames[1]["raw"], mode == "reopen-load-refused")
    if mode != "reopen-load-refused":
        if mode == "reopen-off":
            exchange(
                {"type": "control_request", "request": {
                    "subtype": "apply_flag_settings", "settings": {"effortLevel": "max"},
                }},
                {"type": "control_response", "response": {"subtype": "success", "response": {}}},
            )
        reply = frames[3]["raw"]
        settings = reply["response"]["response"]
        observation = None
        if mode == "reopen-off":
            settings["applied"]["effort"] = "high"
            settings["effective"] = {"autoCompactEnabled": False, "autoCompactWindow": 120000}
        if mode == "reopen-tokens":
            settings["applied"]["effort"] = "unrecognized"
            settings["effective"] = {"autoCompactWindow": 120000}
        if mode == "reopen-malformed":
            settings["effective"] = []
        if mode == "reopen-no-model":
            settings["applied"].pop("model")
        if mode == "reopen-wrong-binding":
            observation = {"type": "system", "subtype": "init", "session_id": str(uuid.uuid4()),
                           "model": "wrong-conversation", "tools": []}
        exchange(frames[2]["raw"], reply, mode == "reopen-settings-refused", observation)
    assert not sys.stdin.readline(), "extra native input after reopen"
    Path("eof").touch()
    sys.exit(0)


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
if mode == "live-settings":
    for settings, refuse in (
        ({"autoCompactEnabled": True, "autoCompactWindow": 120000, "effortLevel": "high"}, False),
        ({"autoCompactEnabled": None, "autoCompactWindow": None}, True),
        ({"autoCompactEnabled": None, "autoCompactWindow": None}, False),
    ):
        exchange(
            {"type": "control_request", "request": {
                "subtype": "apply_flag_settings", "settings": settings,
            }},
            {"type": "control_response", "response": {"subtype": "success", "response": {}}},
            refuse,
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
        print(json.dumps(raw, separators=(",", ":")), flush=True)

    def wait_for_host(index):
        deadline = time.monotonic() + 5
        while not Path(f"continue-{index}").exists():
            assert time.monotonic() < deadline, "caller never received admission"
            time.sleep(0.01)

    if mode.startswith("run-interrupt-"):
        _, _, submit, *rest = frames
        assert select.select([sys.stdin], [], [], 5)[0], "missing user turn"
        turn = json.loads(sys.stdin.readline())
        assert turn == dict(submit["raw"], uuid=turn["uuid"])

        def observe(raw):
            send(raw, submit["raw"]["uuid"], turn["uuid"])

        for entry in rest[:3]:
            observe(entry["raw"])
        if mode == "run-interrupt-compaction":
            observe({"type": "system", "subtype": "status", "status": "compacting"})
        assert select.select([sys.stdin], [], [], 5)[0], "missing interrupt"
        line = sys.stdin.readline()
        request = json.loads(line)
        expected = {"request": {"subtype": "interrupt"},
                    "request_id": request["request_id"], "type": "control_request"}
        assert line == json.dumps(expected, separators=(",", ":")) + "\n"
        Path("interrupt-observed").touch()
        wait_for_host(0)
        receipt, note, result = (entry["raw"] for entry in rest[4:7])
        receipt["response"]["request_id"] = request["request_id"]
        if mode == "run-interrupt-completion":
            result = dict(result, subtype="success", is_error=False, result="done",
                          terminal_reason="completed", errors=[])
        if mode == "run-interrupt-refused":
            receipt["response"] = {"subtype": "error", "request_id": request["request_id"],
                                   "error": "interrupt refused"}
        if mode == "run-interrupt-malformed":
            receipt["response"]["response"] = {"still_queued": 1}
        terminal_first = mode in {"run-interrupt-completion", "run-interrupt-terminal-first",
                                  "run-interrupt-no-receipt"}
        if terminal_first:
            observe(note)
            observe(result)
            if mode == "run-interrupt-no-receipt":
                sys.exit(1)
            Path("terminal-sent").touch()
            wait_for_host(1)
        print(json.dumps(receipt), flush=True)
        if not terminal_first:
            if mode == "run-interrupt-held":
                wait_for_host(1)
            observe(note)
            observe(result)
        observe(result)
        assert select.select([sys.stdin], [], [], 5)[0], "session did not close"
        assert not sys.stdin.readline(), "unexpected second interrupt or next turn"
        Path("eof").touch()
        sys.exit(0)

    if mode.startswith("run-selection-"):
        reason = "Bash is excluded by this run's tool_choice"
        turns = [index for index, entry in enumerate(frames) if entry["action"] == "submit"]
        for run_index, (start, stop) in enumerate(zip(turns, turns[1:] + [len(frames)])):
            assert select.select([sys.stdin], [], [], 5)[0], "missing user turn"
            turn = json.loads(sys.stdin.readline())
            expected = frames[start]["raw"]
            assert turn == dict(expected, uuid=turn["uuid"])
            observations = [entry["raw"] for entry in frames[start:stop] if entry["action"] == "observe"]

            def observe(raw):
                send(raw, expected["uuid"], turn["uuid"])

            def control(subtype, reply):
                request = next(raw for raw in observations if raw.get("request", {}).get("subtype") == subtype)
                observe(request)
                assert select.select([sys.stdin], [], [], 5)[0], "missing native policy reply"
                assert json.loads(sys.stdin.readline()) == {"type": "control_response", "response": {
                    "request_id": request["request_id"], "subtype": "success", "response": reply}}

            denied_by_hook = run_index == 0 and mode == "run-selection-hook"
            if denied_by_hook:
                control("hook_callback", {"hookSpecificOutput": {"hookEventName": "PreToolUse",
                        "permissionDecision": "deny", "permissionDecisionReason": reason}})
            for raw in observations[:3]:
                observe(raw)
            wait_for_host(run_index)
            if denied_by_hook:
                result = next(raw for raw in observations if raw["type"] == "user")
                result["message"]["content"][0].update(content=reason, is_error=True)
                observe(result)
            elif run_index == 0:
                control("can_use_tool", {"behavior": "deny", "message": reason})
            else:
                control("hook_callback", {})
                observe(next(raw for raw in observations if raw["type"] == "user"))
            observe(next(raw for raw in observations if raw["type"] == "result"))
        assert select.select([sys.stdin], [], [], 5)[0], "session did not close"
        assert not sys.stdin.readline()
        Path("eof").touch()
        sys.exit(0)

    if mode.startswith("run-children") or mode == "run-queued-continuation":
        previous_child = None
        run_index = 0
        for entry in frames[2:]:
            action, raw = entry["action"], entry["raw"]
            if action == "submit":
                assert select.select([sys.stdin], [], [], 5)[0], "missing user turn"
                turn = json.loads(sys.stdin.readline())
                corpus_uuid, native_uuid = raw["uuid"], turn["uuid"]
                assert turn == dict(raw, uuid=native_uuid)
            elif action == "observe":
                if mode == "run-children-mixed" and raw["type"] == "result":
                    raw = dict(raw, queued_turn_count=1)
                if mode == "run-children-mixed" and raw.get("subtype") in {"task_notification", "task_updated"}:
                    raw = {"type": "system", "subtype": "session_state_changed", "state": "idle"}
                if raw.get("subtype") == "task_notification":
                    previous_child = raw
                send(raw, corpus_uuid, native_uuid)
            elif action == "drain":
                if previous_child is not None:
                    send(previous_child, corpus_uuid, native_uuid)
                if mode == "run-children-mixed":
                    if run_index == 0:
                        task_done = next(row["raw"] for row in frames if row["raw"].get("subtype") == "task_notification")
                        send(task_done, corpus_uuid, native_uuid)
                elif mode == "run-queued-continuation":
                    interim = frames[6]["raw"]
                    send(dict(interim, queued_turn_count=0, user_message_uuid="foreign",
                              user_message_uuids=[]), corpus_uuid, native_uuid)
                print(json.dumps({"type": "control_request", "request_id": "barrier",
                                  "request": {"subtype": "barrier"}}), flush=True)
                assert select.select([sys.stdin], [], [], 5)[0], "missing drain barrier reply"
                assert json.loads(sys.stdin.readline())["response"]["request_id"] == "barrier"
                Path(f"deferred-{run_index}").touch()
                if mode == "run-children-pending-eof":
                    sys.exit(1)
                if mode == "run-children-live-eof":
                    wait_for_host(run_index)
                    sys.exit(1)
                if mode == "run-children-settled-eof":
                    continue
                wait_for_host(run_index)
            elif action == "wait-run":
                if mode == "run-children-settled-eof":
                    sys.exit(1)
                run_index += 1
        assert select.select([sys.stdin], [], [], 5)[0], "session did not close"
        assert not sys.stdin.readline(), "extra native input"
        Path("eof").touch()
        sys.exit(0)

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
            if raw.get("request", {}).get("subtype") == "can_use_tool":
                permission_ask = raw
            if mode == "run-tools-edges":
                if raw["type"] == "assistant":
                    blocks = raw["message"]["content"]
                    if run_index == 0:
                        blocks.append({"type": "tool_use", "id": "toolu_extra", "name": "Task"})
                    if run_index == 2:
                        blocks.append(blocks[0].copy())
                if raw["type"] == "user" and isinstance(raw["message"]["content"], list):
                    if run_index == 0:
                        raw = dict(raw, parent_tool_use_id="child")
                    if run_index == 1:
                        blocks = raw["message"]["content"]
                        late = dict(blocks[0], tool_use_id="toolu_01")
                        foreign = dict(blocks[0], tool_use_id="foreign")
                        send(dict(raw, origin={"kind": "task-notification"},
                                  message=dict(raw["message"], content=[foreign])),
                             corpus_uuid, native_uuid)
                        blocks[0]["content"] = [{"type": "text", "text": "data"}]
                        blocks.insert(0, late)
            if mode.startswith("run-compaction"):
                if raw.get("status") == "compacting":
                    send(raw, corpus_uuid, native_uuid)
                    wait_for_host(run_index)
                    if mode == "run-compaction-edges":
                        send(dict(raw, uuid=raw["uuid"] + "-repeat"), corpus_uuid, native_uuid)
                    continue
                if mode == "run-compaction-edges" and raw.get("compact_result"):
                    continue
            if mode == "run-buffered" and raw.get("event", {}).get("type") == "message_start":
                buffered.append(raw)
                continue
            if raw["type"] == "result":
                for echo in buffered:
                    send(echo, corpus_uuid, native_uuid)
                if mode != "run-native-buffered" and not mode.startswith("run-compaction"):
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
        elif action == "expect-write" and mode == "run-permission-withdrawal" and raw["response"]["request_id"].startswith("ask-"):
            if run_index == 1:
                send(dict(permission_ask, request_id="overlapping-ask"), corpus_uuid, native_uuid)
            wait_for_host(f"withdraw-{run_index}")
            print(json.dumps({"type": "control_cancel_request",
                              "request_id": raw["response"]["request_id"]}), flush=True)
            if run_index == 1:
                wait_for_host("withdraw-overlapping")
                print(json.dumps({"type": "control_cancel_request",
                                  "request_id": "overlapping-ask"}), flush=True)
            wait_for_host(f"withdrawn-{run_index}")
        elif action == "expect-write" and mode.startswith("run-permission"):
            assert select.select([sys.stdin], [], [], 5)[0], "missing native control reply"
            assert json.loads(sys.stdin.readline()) == raw, raw
        elif action == "decode-error":
            print(raw, flush=True)
            break
        elif action == "expect-write" and mode.startswith("run-tools"):
            assert select.select([sys.stdin], [], [], 5)[0], "missing hook response"
            assert json.loads(sys.stdin.readline()) == raw, raw
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
    if mode == "admission-control-routing":
        def send_control(request_id, subtype, **fields):
            print(json.dumps({"type": "control_request", "request_id": request_id,
                              "request": dict(fields, subtype=subtype)}), flush=True)

        def read_reply(request_id, **fields):
            assert select.select([sys.stdin], [], [], 5)[0], request_id
            reply = json.loads(sys.stdin.readline())
            Path("last-control-reply").write_text(json.dumps(reply))
            assert reply == {"type": "control_response",
                             "response": dict(fields, request_id=request_id)}, reply

        def ask(request_id):
            send_control(request_id, "can_use_tool", tool_name="Read", tool_use_id="tool",
                         input={"file_path": "fixture.txt"})

        send_control("hook", "hook_callback", callback_id="oap_tool_selection",
                     input={"hook_event_name": "PreToolUse"})
        read_reply("hook", subtype="success", response={})
        send_control("invalid-hook", "hook_callback", callback_id="oap_tool_selection",
                     input={"hook_event_name": "PostToolUse"})
        read_reply("invalid-hook", subtype="error", error="claude adapter: unregistered hook callback")
        ask("before-echo")
        read_reply("before-echo", subtype="error",
                   error="claude adapter: permission ask outside an owned run")
        Path("before-echo-replied").touch()

        for index, request_id in ((6, "before-terminal"), (8, "after-terminal")):
            observed = dict(frames[index]["raw"], session_id=binding, user_message_uuid=frame["uuid"],
                            user_message_uuids=[])
            print(json.dumps(observed), flush=True)
            if index == 6:
                ask("withdrawn-before-admission")
                print(json.dumps({"type": "control_cancel_request",
                                  "request_id": "withdrawn-before-admission"}), flush=True)
            ask(request_id)
        send_control("barrier", "barrier")
        read_reply("barrier", subtype="error", error="claude adapter: no owned run")
        Path("controls-buffered").touch()
        read_reply("before-terminal", subtype="error", error="claude adapter: run settled")
        read_reply("after-terminal", subtype="error",
                   error="claude adapter: permission ask outside an owned run")
        assert select.select([sys.stdin], [], [], 5)[0], "session did not close"
        assert not sys.stdin.readline(), "unexpected duplicate reply"
        Path("eof").touch()
        sys.exit(0)
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
