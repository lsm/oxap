import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import queue
import runpy
import shlex
import subprocess
import sys
import tempfile
import threading
import urllib.request
import uuid


SHARED = runpy.run_path(str(Path(__file__).with_name("claude-model-endpoint.py")))
ROOT = SHARED["ROOT"]
VARIABLE = SHARED["VARIABLE"]
exchange = SHARED["exchange"]
PARITY = ROOT / "go/cmd/goap/testdata/parity"
WIRES = {
    "anthropic-messages": ("anthropic-compatible", "messages"),
    "openai-completions": ("openai-compatible", "chat/completions"),
    "openai-responses": ("openai-compatible/responses", "responses"),
}
MODEL = "gateway-model"


def request(base, path, key, wire):
    headers = {"X-Api-Key": key} if wire == "anthropic-messages" else {"Authorization": "Bearer " + key}
    outgoing = urllib.request.Request(base + path, data=b"{}", headers=headers)
    with urllib.request.urlopen(outgoing, timeout=5) as response:
        assert response.status == 204


class OpenCode(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    model = {"id": MODEL, "providerID": "oapx"}
    session = "ses_fake00000000000000"

    def reply(self, data):
        body = json.dumps(data).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/api/info":
            self.reply({"version": self.server.version})
        elif self.path == "/api/event":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b'data: {"id":"evt_connected","type":"server.connected","data":{}}\n\n')
            for seq, (kind, fields) in enumerate(self.server.events.get(timeout=10), 1):
                event = {"id": f"evt_{seq}", "created": seq, "type": "session." + kind,
                         "durable": {"aggregateID": self.session, "seq": seq, "version": 1},
                         "data": {"sessionID": self.session, **fields}}
                self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\n")
            self.close_connection = True
        else:
            self.send_error(404)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/api/session":
            assert body.get("model") == self.model, "OpenCode session did not select the routed model"
            self.reply({"data": {"id": self.session, "model": self.model, "projectID": "prj_fixture",
                                 "time": {"created": 1, "updated": 1}}})
        elif self.path.endswith("/prompt"):
            self.reply({"data": {"id": body["id"], "sessionID": self.session, "type": "user",
                                 "time": {"created": 1}, "payload": {"text": body["text"]},
                                 "delivery": body["delivery"]}})
            key = self.server.key
            request(self.server.base, self.server.path, key, self.server.wire)
            self.server.events.put([
                ("inbox.delivered", {"inboxID": body["id"]}),
                ("step.started", {"assistantMessageID": "msg_a1", "agent": "build", "model": self.model, "started": 1}),
                ("text.ended", {"assistantMessageID": "msg_a1", "ordinal": 0, "text": key}),
                ("step.ended", {"assistantMessageID": "msg_a1", "finish": "stop", "cost": 0,
                                 "tokens": {"input": 2, "output": 5, "reasoning": 0, "cache": {"read": 0, "write": 0}}}),
                ("execution.succeeded", {}),
            ])
        else:
            self.send_error(404)

    def log_message(self, *_):
        pass


def pi_turn(base, path, key, wire):
    def emit(event, **fields):
        print(json.dumps({"type": event, **fields}), flush=True)

    count = 0
    for line in sys.stdin:
        incoming = json.loads(line)
        command = incoming["type"]
        reply = {"id": incoming["id"], "command": command, "success": True}
        if command == "get_state":
            emit("response", **reply, data={
                "thinkingLevel": "off", "steeringMode": "all", "followUpMode": "one-at-a-time",
                "messageCount": count, "pendingMessageCount": 0, "sessionId": "native-session",
                "isStreaming": False, "model": {"id": MODEL, "provider": "oapx"},
            })
            continue
        assert command == "prompt"
        request(base, path, key, wire)
        emit("response", **reply)
        assistant = {"role": "assistant", "content": [{"type": "text", "text": key}],
                     "api": wire, "provider": "oapx", "model": MODEL, "stopReason": "stop",
                     "usage": {"input": 1, "output": 1, "totalTokens": 2}, "timestamp": 1}
        emit("agent_start")
        emit("turn_start")
        emit("message_start", message={**assistant, "content": []})
        emit("message_update", assistantMessageEvent={"type": "text_delta", "contentIndex": 0, "delta": key}, usage={})
        emit("message_end", message=assistant)
        emit("turn_end", message=assistant, toolResults=[])
        emit("agent_end", messages=[assistant], willRetry=False)
        emit("agent_settled")
        count = 2


def child(kind, wire, base, fixture, args):
    package, path = WIRES[wire]
    if kind == "opencode":
        config = json.loads(os.environ["OPENCODE_CONFIG_CONTENT"])
        provider = config["providers"]["oapx"]
        assert provider["package"] == "@opencode/ai/providers/" + package
        assert provider["settings"] == {"baseURL": base, "apiKey": "{env:" + VARIABLE + "}"}
        assert MODEL in provider["models"]
        key = os.environ[VARIABLE]
        assert key not in os.environ["OPENCODE_CONFIG_CONTENT"]
        port = int(args[args.index("--port") + 1])
        with http.server.ThreadingHTTPServer(("127.0.0.1", port), OpenCode) as server:
            version = json.loads((ROOT / "harnesses/opencode.json").read_text())
            server.version = next(v["admits"][0] for v in version["versions"] if v["status"] == "current")
            server.base, server.wire, server.path, server.key = base, wire, path, key
            server.events = queue.Queue()
            server.serve_forever()
        return
    if kind == "pi":
        assert args[:5] == ["--no-session", "--provider", "oapx", "--model", MODEL]
        directory = Path(os.environ["PI_CODING_AGENT_DIR"])
        assert directory.parent == Path.cwd().parent / "pi-home/.oxap/pi"
        rendered = (directory / "models.json").read_bytes()
        assert directory.name == hashlib.sha256(rendered).hexdigest()
        provider = json.loads(rendered)["providers"]["oapx"]
        assert provider["baseUrl"] == base and provider["api"] == wire
        assert provider["apiKey"] == "${" + VARIABLE + "}"
        assert [model["id"] for model in provider["models"]] == [MODEL]
        key = os.environ[VARIABLE]
        assert key.encode() not in rendered
        assert all(key not in arg for arg in args)
        os.write(2, b"native stderr: " + key.encode() + b"\n")
        pi_turn(base, path, key, wire)
        return
    elif kind == "copilot":
        assert os.environ["COPILOT_PROVIDER_BASE_URL"] == base
        key = os.environ["COPILOT_PROVIDER_API_KEY"]
    else:
        assert args[:2] == ["serve", "acp"]
        config = Path(args[2]).read_text()
        assert f"base_url: {base}" in config
        assert "api_type: openai_chatcompletions" in config
        assert f"token_key: {VARIABLE}" in config
        key = os.environ[VARIABLE]
        assert key not in config
    assert all(key not in arg for arg in args)
    request(base, path, key, wire)
    os.execv("/bin/sh", ["/bin/sh", fixture])


def verify_refusals(command, config, entry, env, kind, sentinel):
    route = entry["model_endpoint"]
    routes = [
        ({"model_endpoint": {k: v for k, v in route.items() if k != "wire"}}, "wire"),
        ({"model_endpoint": {**route, "wire": "invalid"}}, "wire"),
        ({"model": ""}, "oapx/<model-id>"),
        ({"model": "oapx/"}, "oapx/<model-id>"),
        ({"model": "other/gateway-model"}, "oapx/<model-id>"),
        ({"environment": [VARIABLE]}, VARIABLE),
    ]
    if kind == "opencode":
        routes.extend([
            ({"environment": ["OPENCODE_CONFIG"]}, "OPENCODE_CONFIG"),
            ({"environment": ["OPENCODE_CONFIG_CONTENT"]}, "OPENCODE_CONFIG_CONTENT"),
            ({"endpoint": "http://127.0.0.1:1"}, "names an endpoint and a model_endpoint"),
        ])
    else:
        routes.extend([
            ({"environment": ["PI_CODING_AGENT_DIR"]}, "PI_CODING_AGENT_DIR"),
            ({"args": ["--model", "wrong"]}, "--model"),
            ({"args": ["--provider=wrong"]}, "--provider"),
            ({"args": ["--api-key", "argv-marker"]}, "--api-key"),
            ({"args": ["--api-key=argv-marker"]}, "--api-key"),
        ])
    for override, diagnostic in routes:
        config.write_text(json.dumps({"adapters": {"routed": {**entry, **override}}}))
        refused = subprocess.run(command, env=env, input=b"", capture_output=True, timeout=10)
        assert sentinel.encode() not in refused.stdout + refused.stderr
        assert b"argv-marker" not in refused.stdout + refused.stderr
        assert refused.returncode != 0 and b'"routed"' in refused.stderr, f"{kind} did not refuse {diagnostic}"
        assert diagnostic.encode() in refused.stderr, f"{kind} refusal lacks {diagnostic}"
    print(f"{kind}: {len(routes)} named startup refusals with a nonzero exit")


def verify(binary, goap, directory, kind, wire, base, sentinel):
    fixture = PARITY / kind
    config = directory / "hub.json"
    child_file = directory / "child.sh"
    launcher = directory / "launcher"
    home = directory.parent / "pi-home" if kind == "pi" else directory / "home"
    home.mkdir(exist_ok=True)
    if kind in ("copilot", "acp"):
        script = (fixture / "child.sh").read_text()
        if kind == "acp":
            lines = script.splitlines(keepends=True)
            script = "".join(line for line in lines if not any(
                token in line for token in ("tool_call", "session/request_permission")))
            script = script.replace("take\ntake\n", "take\n")
        child_file.write_text(script)
    argv = [sys.executable, str(Path(__file__).resolve()), "--child", kind, wire, base, str(child_file)]
    launcher.write_text("#!/bin/sh\nexec " + shlex.join(argv) + ' "$@"\n')
    launcher.chmod(0o700)
    entry = {"type": kind, "executable": str(launcher), "working_directory": str(directory), "environment": ["PATH"]}
    env = {"PATH": os.environ["PATH"], "HOME": str(home), "NODE_ENV": "test"}
    if kind in ("opencode", "pi"):
        entry.update(model="oapx/" + MODEL, model_endpoint={"base_url": base, "credential_env": VARIABLE, "wire": wire})
        env[VARIABLE] = sentinel
        if kind == "pi":
            entry["args"] = ["--no-session"]
    elif kind == "copilot":
        entry["environment"].extend(["COPILOT_PROVIDER_BASE_URL", "COPILOT_PROVIDER_API_KEY"])
        env.update(COPILOT_PROVIDER_BASE_URL=base, COPILOT_PROVIDER_API_KEY=sentinel)
    else:
        yaml = directory / "agent.yaml"
        yaml.write_text(f"providers:\n  fixture:\n    base_url: {base}\n    api_type: openai_chatcompletions\n"
                        f"    token_key: {VARIABLE}\nmodels:\n  gateway:\n    provider: fixture\n"
                        f"    model: {MODEL}\nagents:\n  root:\n    model: gateway\n")
        entry["args"] = ["serve", "acp", str(yaml)]
        entry["environment"].extend([VARIABLE, "LIST_ME=from-adapter"])
        env[VARIABLE] = sentinel
    config.write_text(json.dumps({"adapters": {"routed": entry}}))
    command = [str(binary), "serve", "--stdio", "--no-save-oapx-sessions", "--config", str(config)]
    if kind == "copilot":
        command = [str(goap), "hub", "--stdio", "--config", str(config)]
    frames = [json.loads(line) for line in (fixture / "scenario.jsonl").read_bytes().split(b"\n") if line]
    opening, submit = frames[3 if kind == "acp" else 2], next(
        f for f in frames if f["type"] == "session.message.submit.request")
    if kind == "acp":
        opening["payload"].pop("tool_sources")
    with tempfile.TemporaryFile() as stderr, subprocess.Popen(
        command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0,
    ) as hub:
        try:
            observed = exchange(hub, [{"id": 1, "op": "open", "adapter": "routed", "request": opening}],
                                lambda line: line.get("id") == 1)
            assert b"session.open.response" in observed, f"{kind}: routed session did not open"
            observed += exchange(hub, [{"id": 2, "op": "events", "session_id": "s1"}],
                                 lambda line: line.get("id") == 2)
            observed += exchange(hub, [{"id": 3, "op": "submit", "session_id": "s1", "request": submit}],
                                 lambda line: (line.get("id") == 3 and not line.get("ok")) or
                                 any(kind in json.dumps(line) for kind in ("run.completed", "run.failed")))
            assert b"run.completed" in observed, f"{kind}: routed turn did not complete"
            observed += exchange(hub, [{"id": 4, "op": "close", "session_id": "s1"}],
                                 lambda line: line.get("id") == 4)
            tail, _ = hub.communicate(timeout=10)
            observed += tail
            assert hub.returncode == 0, f"hub exit={hub.returncode}"
        except AssertionError:
            stderr.seek(0)
            if b"has no custom endpoint translation" in stderr.read():
                raise AssertionError(f"{kind}: hub refused model_endpoint as unsupported") from None
            raise
        finally:
            if hub.poll() is None:
                hub.kill()
                hub.wait(timeout=10)
        stderr.seek(0)
        logs = stderr.read()
        assert sentinel.encode() not in observed + logs, "credential escaped into hub output"
        if kind in ("opencode", "pi"):
            assert any(b"content.delta" in line and VARIABLE.encode() in line for line in observed.splitlines())
        if kind == "pi":
            assert f"native stderr: {VARIABLE}".encode() in logs
    print(f"{kind} {wire}: hub exit=0 and one completed routed turn")
    if kind in ("opencode", "pi") and wire == "openai-completions":
        verify_refusals(command, config, entry, env, kind, sentinel)


def main():
    if sys.argv[1:2] == ["--child"]:
        child(*sys.argv[2:6], sys.argv[6:])
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--goap", type=Path, required=True)
    args = parser.parse_args()
    sentinel = uuid.uuid4().hex
    with http.server.HTTPServer(("127.0.0.1", 0), SHARED["Endpoint"]) as server:
        server.observed = []
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        expected = []
        try:
            with tempfile.TemporaryDirectory(prefix="oxap-route-") as directory:
                root = Path(directory).resolve()
                for kind in ("opencode", "pi", "copilot", "acp"):
                    for wire in WIRES if kind in ("opencode", "pi") else ("openai-completions",):
                        path = f"/{kind}/" + WIRES[wire][1]
                        expected.append((path, sentinel, None) if wire == "anthropic-messages" else
                                        (path, None, "Bearer " + sentinel))
                        case = root / f"{kind}-{wire}"
                        case.mkdir()
                        verify(args.binary.resolve(), args.goap.resolve(), case, kind, wire,
                               f"http://127.0.0.1:{server.server_port}/{kind}/", sentinel)
                pi_files = list((root / "pi-home/.oxap/pi").glob("*/models.json"))
                assert len(pi_files) == len(WIRES), "Pi wires shared a generated file"
                assert all(sentinel.encode() not in p.read_bytes() for p in pi_files)
                print(f"Pi: {len(pi_files)} distinct generated files under one state directory")
        finally:
            server.shutdown()
            worker.join()
        assert server.observed == expected, "stub endpoint saw the wrong paths or authentication"
        print(f"PASS: stub upstream observed {len(server.observed)} requests")


if __name__ == "__main__":
    main()
