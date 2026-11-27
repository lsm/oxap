import argparse
from contextlib import ExitStack
from http.server import ThreadingHTTPServer
import json
from pathlib import Path
import runpy
import secrets
import tempfile
import threading
import time

CREATE = runpy.run_path(str(Path(__file__).with_name("codex-session-tool-sources.py")))
HUB = CREATE["HUB"]
PROMPT = "Verify the bound tool after a"
REPLY = "Bound tool verified"
SOURCE = {"id": "tools", "kind": "remote"}


class Responses(CREATE["FixtureServer"]):
    def do_POST(self):
        if self.path != "/model/responses":
            return super().do_POST()
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.model_requests.append(body)
        index = len(self.server.model_requests)
        message = {"type": "message", "id": f"msg_{index}", "role": "assistant",
                   "status": "completed", "content": [{"type": "output_text", "text": REPLY,
                                                        "annotations": []}]}
        response = {"id": f"resp_{index}", "status": "completed", "output": [message],
                    "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}
        events = [
            {"type": "response.created", "response": {"id": response["id"]}},
            {"type": "response.output_item.added", "output_index": 0, "item": {**message, "content": []}},
            {"type": "response.output_text.delta", "output_index": 0, "item_id": message["id"],
             "content_index": 0, "delta": REPLY},
            {"type": "response.output_item.done", "output_index": 0, "item": message},
            {"type": "response.completed", "response": response},
        ]
        data = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def verify(binary, codex, root, server, stack):
    grants = root / "grants"
    grants.mkdir(mode=0o700)
    home = root / "home"
    home.mkdir()
    codex_home = root / "codex-home"
    CREATE["native_config"](codex_home, server.server_port)
    codex_config = codex_home / "config.toml"
    codex_config.write_text('model = "fixture"\n' + codex_config.read_text())
    config = root / "hub.json"
    settings = {"adapters": {"codex": {
        "type": "codex", "executable": str(codex), "model": "fixture",
        "working_directory": str(root), "approval_policy": "never", "sandbox": "read-only",
        "environment": [f"HOME={home}", f"CODEX_HOME={codex_home}", "PATH=/usr/bin:/bin", "NODE_ENV=test"],
    }}}
    config.write_text(json.dumps(settings))
    bindings = root / "bindings.jsonl"
    command = [str(binary), "serve", "--stdio", f"--config={config}", "--park-after", "2",
               "--session-tool-sources", str(grants), "--session-history", str(bindings),
               "--no-save-oapx-sessions"]
    canaries = []

    def start(label):
        hub = HUB["Hub"](command, root, label)
        stack.callback(hub.cleanup)
        return hub

    def grant(label, session="bound", source=SOURCE):
        reference, bearer = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        canaries.extend((reference, bearer))
        authorization = "Bearer " + bearer
        server.routes[label] = authorization
        path = grants / reference
        path.touch(mode=0o600)
        path.write_text(json.dumps({
            "session_id": session, "headers": {"Authorization": authorization},
            "source": {**source, "protocol": "mcp", "endpoint": f"http://127.0.0.1:{server.server_port}/{label}"},
        }))
        return {**source, "credential_ref": reference}

    def open_session(hub, reopen, sources=None):
        payload = {"session_id": "bound", "reopen": reopen, "reasoning_level": "high"}
        if sources is not None:
            payload["tool_sources"] = sources
        return hub.call("open", adapter="codex", request=HUB["envelope"]("session.open.request", payload))

    def turn(hub, phase):
        response = hub.call("submit", session_id="bound", request=HUB["envelope"](
            "session.message.submit.request", {"session_id": "bound", "delivery": "auto",
                "messages": [{"role": "user", "content": f"{PROMPT} {phase}."}]}))
        assert response["ok"], f"submit failed for {phase}"
        run = response["result"]["payload"]["run_id"]
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            for frame in hub.seen:
                event = frame.get("envelope", {})
                if event.get("payload", {}).get("run_id") != run:
                    continue
                if event.get("type") in {"run.completed", "run.failed", "run.cancelled"}:
                    assert event["type"] == "run.completed" and REPLY in json.dumps(event), f"turn failed for {phase}"
                    return run
            frame = hub.lines.get(timeout=max(0.1, deadline - time.monotonic()))
            assert frame is not None, f"hub exited during {phase}"
            hub.seen.append(frame)
        raise AssertionError(f"turn did not settle for {phase}")

    original = grant("original")
    first = start("before")
    assert open_session(first, False, [original])["ok"], "create failed"
    assert first.call("events", session_id="bound")["ok"]
    turn(first, "create")
    first.stop()
    print("create turn completed; original hub exited 0", flush=True)
    del server.routes["original"]

    settings["tool_sources"] = {"tools": {"kind": "remote", "protocol": "mcp",
                                          "endpoint": f"http://127.0.0.1:{server.server_port}/registry"}}
    config.write_text(json.dumps(settings))
    second = start("after")
    fresh = grant("rotated")
    other_source = {**SOURCE, "id": "other"}
    extra = grant("extra", source=other_source)
    foreign = grant("foreign", session="another")
    refused = [(None, "tools"), ([], "tools"), ([SOURCE], "tools"), ([original], "tools"),
               ([{**SOURCE, "credential_ref": "invalid"}], "tools"), ([foreign], "tools"),
               ([extra], "other"), ([fresh, extra], "other"), ([fresh, fresh], "tools")]
    for sources, source in refused:
        error = open_session(second, True, sources).get("error", {})
        assert error.get("code") == "unsupported_feature" and error.get("details") == {
            "feature": CREATE["ATTACH"], "reason": "unsatisfiable", "source": source,
        }, f"reopen with {source} did not refuse the bound source mismatch"
    assert (grants / fresh["credential_ref"]).is_file(), "refused reopen consumed the fresh grant"
    reopened = open_session(second, True, [fresh])
    assert reopened["ok"], "fresh grant did not reopen"
    state = reopened["result"]["payload"]
    assert state["recovery"]["recovered"] and [source["id"] for source in state["sources"]] == ["tools"]
    assert not (grants / fresh["credential_ref"]).exists(), "reopen left a reusable grant"
    assert second.call("events", session_id="bound")["ok"]
    run = turn(second, "restart")

    time.sleep(3)
    assert second.call("events", session_id="bound", run_id=run, after=0)["ok"]
    gap = second.lines.get(timeout=5)
    assert gap.get("event") == "oap-replay-gap", "idle session with bound tools did not park"
    second.seen.append(gap)
    turn(second, "wake")
    assert second.call("close", session_id="bound")["ok"]
    replay = open_session(second, True, [fresh]).get("error", {})
    assert replay.get("code") == "unsupported_feature" and replay.get("details") == {
        "feature": CREATE["ATTACH"], "reason": "unsatisfiable", "source": "tools",
    }, "a consumed reopen grant could be reused after close"
    second.stop()

    requests = server.model_requests
    assert len(requests) == 3, f"expected three model turns, saw {len(requests)}"
    for request, (phase, label) in zip(requests, (("create", "original"), ("restart", "rotated"), ("wake", "rotated"))):
        tools = json.dumps(request.get("tools", []))
        assert f"{PROMPT} {phase}." in json.dumps(request["input"])
        assert f'{label}_tool"' in tools, f"missing {label} tool after {phase}; MCP requests={sorted(server.mcp_methods)}"
        assert sum(f'{name}_tool"' in tools for name in ("original", "rotated", "extra", "foreign")) == 1
    resumed_input = json.dumps(requests[1]["input"])
    assert f"{PROMPT} create." in resumed_input and REPLY in resumed_input, "Codex did not restore the prior model turn"
    disk = bindings.read_text()
    records = [json.loads(line.split(" ", 1)[1])["record"] for line in disk.splitlines()]
    assert records and all(record["tool_source_ids"] == ["tools"] for record in records)
    assert all(record["credential_tool_source_ids"] == ["tools"] for record in records)
    assert len({record["native_session_id"] for record in records}) == 1
    files = [path for path in codex_home.rglob("*") if path.is_file()]
    assert sum(path.name.startswith("rollout-") for path in files) == 1
    published = [disk.encode(), json.dumps(first.seen + second.seen).encode()]
    published.extend((root / f"{label}-stderr.txt").read_bytes() for label in ("before", "after"))
    assert not any(canary.encode() in data for data in published + [path.read_bytes() for path in files]
                   for canary in canaries), "published credential material"
    print(f"PASS: hub exits=0,0; model turns=3; refused reopens={len(refused) + 1}; idle wakes=1; "
          f"rollouts=1; Codex home files={len(files)}; canary hits=0")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--codex", required=True, type=Path)
    args = parser.parse_args()
    with ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="oap-codex-reopen-"))).resolve()
        server = stack.enter_context(ThreadingHTTPServer(("127.0.0.1", 0), Responses))
        server.routes = {}
        server.model_requests = []
        server.mcp_methods = set()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.shutdown)
        verify(args.binary.resolve(), args.codex.resolve(), root, server, stack)
