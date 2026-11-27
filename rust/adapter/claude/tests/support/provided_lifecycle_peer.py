#!/usr/bin/env python3
import json
import os
from pathlib import Path
import select
import sys
import time

assert os.environ["NODE_ENV"] == "test"
assert Path(os.environ["HOME"]) == Path.cwd()
assert set(os.environ) <= {"HOME", "PATH", "MODE", "NODE_ENV", "LC_CTYPE"}
mode = os.environ["MODE"]
binding = sys.argv[sys.argv.index("--session-id") + 1]
qualified = "mcp__oap__echo"


def read():
    assert select.select([sys.stdin], [], [], 5)[0], "missing host frame"
    return json.loads(sys.stdin.readline())


def send(kind, **fields):
    print(json.dumps({"type": kind, **fields}), flush=True)


def observe(kind, **fields):
    send(kind, session_id=binding, **fields)


def control(identifier, request, expected):
    send("control_request", request_id=identifier, request=request)
    assert read() == {"type": "control_response", "response": {
        "subtype": "success", "request_id": identifier, "response": expected,
    }}
    Path(identifier).touch()


def mcp_call(identifier):
    send("control_request", request_id=identifier, request={
        "subtype": "mcp_message", "server_name": "oap", "message": {
            "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
                "name": "echo", "arguments": {"text": "hi"},
                "_meta": {"claudecode/toolUseId": "toolu_echo"},
            },
        },
    })


def expect_mcp(identifier, **payload):
    assert read() == {"type": "control_response", "response": {
        "subtype": "success", "request_id": identifier, "response": {
            "mcp_response": {"jsonrpc": "2.0", "id": 7, **payload},
        },
    }}


initialize = read()
assert initialize["request"]["subtype"] == "initialize"
assert initialize["request"]["sdkMcpServers"] == ["oap"]
send("control_response", response={
    "subtype": "success", "request_id": initialize["request_id"], "response": {},
})
turn = read()
assert turn["type"] == "user" and turn["message"]["content"] == "go"
uuid = turn["uuid"]
observe("system", subtype="init", user_message_uuid=uuid, model="fixture",
        tools=[qualified], mcp_servers=[{"name": "oap"}])
observe("assistant", uuid="assistant", parent_tool_use_id=None,
        message={"id": "msg", "role": "assistant", "model": "fixture", "content": [
            {"type": "tool_use", "id": "toolu_echo", "name": qualified, "input": {"text": "hi"}},
        ]})
if mode in {"held", "cancel-held"}:
    mcp_call("call")
    control("barrier", {"subtype": "mcp_message", "server_name": "oap", "message": {
        "jsonrpc": "2.0", "id": 8, "method": "ping",
    }}, {"mcp_response": {"jsonrpc": "2.0", "id": 8, "result": {}}})
if mode == "denied":
    reason = "echo is excluded by this run's tool_choice"
    control("hook", {"subtype": "hook_callback", "callback_id": "oap_tool_selection", "input": {
        "hook_event_name": "PreToolUse", "tool_name": qualified, "tool_use_id": "toolu_echo",
    }}, {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
                                "permissionDecisionReason": reason}})
    control("gate", {"subtype": "can_use_tool", "tool_name": qualified, "tool_use_id": "toolu_echo",
                     "input": {}}, {"behavior": "deny", "message": reason})
else:
    deadline = time.monotonic() + 5
    while not Path("continue").exists():
        assert time.monotonic() < deadline, "caller did not receive the open tool interaction"
        time.sleep(0.01)
if mode == "reply-first":
    mcp_call("call")
if mode in {"held", "reply-first"}:
    text = "hi back" if mode == "held" else "could not echo"
    expect_mcp("call", result={"content": [{"type": "text", "text": text}],
                               "isError": mode == "reply-first"})
    mcp_call("duplicate")
    expect_mcp("duplicate", error={"code": -32602,
                                  "message": "no provided call matches this tools/call"})
    Path("reply").touch()
if mode in {"native-result", "denied"}:
    for index in range(2):
        observe("user", uuid=f"native-result-{index}", parent_tool_use_id=None,
                message={"role": "user", "content": [{"type": "tool_result",
                         "tool_use_id": "toolu_echo", "content": "native refusal", "is_error": True}]})
observe("result", user_message_uuid=uuid, subtype="success", is_error=False,
        terminal_reason="completed", result="done", queued_turn_count=0)
if mode == "cancel-held":
    expect_mcp("call", error={"code": -32603, "message": "the call was cancelled"})
    Path("reply").touch()
assert select.select([sys.stdin], [], [], 5)[0], "session did not close"
assert not sys.stdin.readline()
Path("eof").touch()
