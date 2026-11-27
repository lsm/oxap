#!/usr/bin/env python3
import json
import os
from pathlib import Path
import select
import sys

assert os.environ["NODE_ENV"] == "test"
assert Path(os.environ["HOME"]) == Path.cwd()
assert set(os.environ) <= {"HOME", "PATH", "MODE", "NODE_ENV", "LC_CTYPE"}
mode = os.environ["MODE"]
args = sys.argv[1:]
binding_at = args.index("--session-id")
expected_args = ["mcp__oap__echo", "mcp__oap__look"] if mode != "empty" else []
if mode == "allow":
    assert args[args.index("--tools") + 1] == "Read"
    expected_args.insert(0, "Read")
if expected_args:
    assert args[args.index("--allowedTools") + 1:binding_at] == expected_args, args
else:
    assert "--allowedTools" not in args, args


def read():
    assert select.select([sys.stdin], [], [], 5)[0], "missing host reply"
    return json.loads(sys.stdin.readline())


def send(frame):
    print(json.dumps(frame, separators=(",", ":")), flush=True)


def discovery(server, message, expected=None):
    send({"type": "control_request", "request_id": "mcp-discovery", "request": {
        "subtype": "mcp_message", "server_name": server, "message": message,
    }})
    reply = read()
    assert reply["type"] == "control_response", reply
    assert reply["response"]["request_id"] == "mcp-discovery", reply
    if expected is None:
        assert reply["response"]["subtype"] == "error", reply
    else:
        assert reply["response"] == {"request_id": "mcp-discovery", "subtype": "success",
                                     "response": expected}, reply


def rpc(method, identifier, *, params=None, result=None, error=None):
    message = {"method": method, "jsonrpc": "2.0", "id": identifier}
    if params is not None:
        message["params"] = params
    reply = {"jsonrpc": "2.0", "id": identifier}
    reply.update({"error": error} if error is not None else {"result": result})
    discovery("oap", message, {"mcp_response": reply})


initialize = read()
assert initialize["request"]["subtype"] == "initialize", initialize
if mode == "empty":
    assert "sdkMcpServers" not in initialize["request"], initialize
    discovery("oap", {"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
else:
    assert initialize["request"]["sdkMcpServers"] == ["oap"], initialize
    server = {"capabilities": {"tools": {}}, "serverInfo": {"name": "oap", "version": "0.1"}}
    rpc("initialize", 0, params={"protocolVersion": "2025-06-18"},
        result=dict(server, protocolVersion="2025-06-18"))
    rpc("initialize", None, result=dict(server, protocolVersion="2025-11-25"))
    discovery("oap", {"jsonrpc": "2.0", "method": "notifications/initialized"}, {})
    rpc("tools/list", 1, result={"tools": [
        {"name": "echo", "description": "echo it", "inputSchema": {
            "type": "object", "properties": {"text": {"type": "string"}},
        }},
        {"name": "look", "inputSchema": {"type": "object"}},
    ]})
    rpc("resources/list", 2, error={"code": -32601, "message": "method not found"})
    rpc("ping", "p", result={})
    discovery("other", {"jsonrpc": "2.0", "id": 3, "method": "tools/list"})
    tools = ["echo", "mcp__oap__look", "mcp__oap__echo", "mcp__oap__echo", "look"]
    send({"type": "system", "subtype": "init", "session_id": args[binding_at + 1],
          "model": "mock-model", "tools": tools,
          "mcp_servers": [{"name": "oap"}]})

Path("discovered").touch()
send({"type":"control_response", "response": {
    "subtype":"success", "request_id": initialize["request_id"], "response": {},
}})
assert select.select([sys.stdin], [], [], 5)[0], "session did not close"
assert not sys.stdin.readline()
Path("eof").touch()
