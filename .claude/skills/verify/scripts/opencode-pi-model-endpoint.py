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
    session = "ses_fixture"

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
        rendered = (directory / "models.json").read_bytes()
        assert directory.name == hashlib.sha256(rendered).hexdigest()
        provider = json.loads(rendered)["providers"]["oapx"]
        assert provider["baseUrl"] == base and provider["api"] == wire
        assert provider["apiKey"] == "${" + VARIABLE + "}"
        assert [model["id"] for model in provider["models"]] == [MODEL]
        key = os.environ[VARIABLE]
        assert key.encode() not in rendered
        os.write(2, b"native stderr: " + key.encode() + b"\n")
    elif kind == "copilot":
        assert os.environ["COPILOT_PROVIDER_BASE_URL"] == base
        key = os.environ["COPILOT_PROVIDER_API_KEY"]
    else:
        config = Path(args[0]).read_text()
        assert f"base_url: {base}" in config
        assert "api_type: openai_chatcompletions" in config
        assert f"token_key: {VARIABLE}" in config
        key = os.environ[VARIABLE]
        assert key not in config
    assert all(key not in arg for arg in args)
    request(base, path, key, wire)
    os.execv("/bin/sh", ["/bin/sh", fixture])


def verify(binary, goap, directory, kind, wire, base, sentinel):
    fixture = PARITY / kind
    config = directory / "hub.json"
    child_file = directory / "child.sh"
    launcher = directory / "launcher"
    home = directory / "home"
    home.mkdir()
    if kind != "opencode":
        script = (fixture / "child.sh").read_text()
        if kind == "pi":
            script = script.replace('"delta":"hel"', '"delta":"\'"$' + VARIABLE + '"\'"', 1)
        if kind == "acp":
            lines = script.splitlines(keepends=True)
            script = "".join(line for index, line in enumerate(lines) if index not in range(6, 10))
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
        entry["args"] = [str(yaml)]
        entry["environment"].extend([VARIABLE, "LIST_ME=from-adapter"])
        env[VARIABLE] = sentinel
    config.write_text(json.dumps({"adapters": {"routed": entry}}))
    command = [str(binary), "serve", "--stdio", "--no-save-oapx-sessions", "--config", str(config)]
    if kind == "copilot":
        command = [str(goap), "hub", "--stdio", "--config", str(config)]
    frames = [json.loads(line) for line in (fixture / "scenario.jsonl").read_text().splitlines()]
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
                                 lambda line: b"run.completed" in json.dumps(line).encode())
            hub.stdin.close()
            assert hub.wait(timeout=10) == 0, f"hub exit={hub.returncode}"
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
            for kind in ("opencode", "pi", "copilot", "acp"):
                for wire in WIRES if kind in ("opencode", "pi") else ("openai-completions",):
                    path = f"/{kind}/" + WIRES[wire][1]
                    expected.append((path, sentinel, None) if wire == "anthropic-messages" else
                                    (path, None, "Bearer " + sentinel))
                    with tempfile.TemporaryDirectory(prefix="oxap-route-") as directory:
                        verify(args.binary.resolve(), args.goap.resolve(), Path(directory).resolve(), kind, wire,
                               f"http://127.0.0.1:{server.server_port}/{kind}/", sentinel)
        finally:
            server.shutdown()
            worker.join()
        assert server.observed == expected, "stub endpoint saw the wrong paths or authentication"
        print(f"PASS: stub upstream observed {len(server.observed)} requests")


if __name__ == "__main__":
    main()
