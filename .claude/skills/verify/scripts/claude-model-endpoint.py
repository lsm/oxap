import argparse
import http.server
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
import uuid


ROOT = Path(__file__).resolve().parents[4]
VARIABLE = "ROUTE_FIXTURE_VALUE"


def child():
    base = os.environ["ANTHROPIC_BASE_URL"]
    assert VARIABLE not in os.environ
    assert not any(arg.startswith(base) for arg in sys.argv)
    if "ANTHROPIC_AUTH_TOKEN" in os.environ:
        assert "ANTHROPIC_API_KEY" not in os.environ
        key = os.environ["ANTHROPIC_AUTH_TOKEN"]
        headers = {"Authorization": "Bearer " + key}
    else:
        key = os.environ["ANTHROPIC_API_KEY"]
        headers = {"X-Api-Key": key}
    assert all(key not in arg for arg in sys.argv)
    request = urllib.request.Request(base + "messages", data=b"{}", headers=headers)
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status == 204
    os.write(2, b"native stderr: " + key[:7].encode())
    time.sleep(0.05)
    os.write(2, key[7:].encode() + b"\n")

    def emit(kind, **fields):
        print(json.dumps({"type": kind, **fields}), flush=True)

    initialize = json.loads(sys.stdin.readline())
    emit("control_response", response={"subtype": "success", "request_id": initialize["request_id"], "response": {}})
    for index, line in enumerate(sys.stdin, 1):
        turn = json.loads(line)
        emit("system", subtype="init", session_id="native-1", tools=[], model="claude-fake", uuid=f"i{index}")
        fields = {"session_id": "native-1", "parent_tool_use_id": None, "user_message_uuid": turn["uuid"]}
        emit("stream_event", event={"type": "message_start"}, uuid=f"e{index}", **fields)
        if index == 1:
            emit("stream_event", event={"type": "content_block_delta", "index": 0,
                 "delta": {"type": "text_delta", "text": f"native output: {key}"}}, uuid="delta", **fields)
            emit("result", subtype="success", is_error=False, stop_reason="end_turn", result="done",
                 uuid="success", **fields)
        else:
            emit("result", subtype="error_during_execution", is_error=True,
                 errors=[f"native failure: {key}"], uuid="failure", **fields)


class Endpoint(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.server.observed.append((self.path, self.headers.get("X-Api-Key"), self.headers.get("Authorization")))
        self.send_response(204)
        self.end_headers()

    def log_message(self, *_):
        pass


def exchange(process, messages, last):
    for message in messages:
        process.stdin.write((json.dumps(message) + "\n").encode())
    lines = []
    deadline = time.monotonic() + 10
    while True:
        assert select.select([process.stdout], [], [], max(0, deadline - time.monotonic()))[0], "hub timed out"
        line = process.stdout.readline()
        assert line, f"hub closed its output (exit={process.poll()})"
        lines.append(line)
        if last(json.loads(line)):
            return b"".join(lines)


def verify_http(command, environment, opening, submit, key):
    command = [item if item != "--stdio" else "--addr=127.0.0.1:0" for item in command]
    with tempfile.TemporaryFile() as stderr, subprocess.Popen(
        command, env=environment, stdout=subprocess.PIPE, stderr=stderr, stdin=subprocess.DEVNULL, bufsize=0,
    ) as hub:
        try:
            assert select.select([hub.stdout], [], [], 10)[0], "HTTP hub did not announce its address"
            ready = hub.stdout.readline().decode().strip()
            assert ready.startswith("listening on http://127.0.0.1:"), "HTTP hub did not bind"
            base = ready.removeprefix("listening on ")

            def request(path, payload=None):
                data = json.dumps(payload).encode() if payload is not None else None
                return urllib.request.urlopen(urllib.request.Request(
                    base + path, data=data, headers={"Content-Type": "application/json"}), timeout=10)

            with request("/adapters/routed/sessions", opening) as response:
                assert json.load(response)["type"] == "session.open.response"
            observed = b""
            with request("/sessions/s1/events") as stream:
                for index, terminal in enumerate(("run.completed", "run.failed"), 4):
                    submit["id"] = f"submit-{index}"
                    with request("/sessions/s1/submit", submit) as response:
                        assert json.load(response)["type"] == "session.message.submit.response"
                    for _ in range(50):
                        line = stream.readline()
                        assert line, "HTTP event stream ended before the terminal event"
                        if line.startswith(b"data: "):
                            observed += line
                            if json.loads(line.removeprefix(b"data: ")).get("type") == terminal:
                                break
                    else:
                        raise AssertionError("HTTP event stream missed the terminal event")
            assert key.encode() not in observed, "sentinel escaped into HTTP events"
            assert f"native output: {VARIABLE}".encode() in observed, "HTTP output lacks the replacement"
            assert f"native failure: {VARIABLE}".encode() in observed, "HTTP failure lacks the replacement"
            hub.terminate()
            assert hub.wait(timeout=10) == 0, f"HTTP hub exit={hub.returncode}"
        finally:
            if hub.poll() is None:
                hub.kill()
                hub.wait(timeout=10)
        stderr.seek(0)
        logs = stderr.read()
        assert key.encode() not in logs, "sentinel escaped into HTTP hub logs"
        assert f"native stderr: {VARIABLE}".encode() in logs, "HTTP hub logs lack the replacement"
        print(f"HTTP: hub exit={hub.returncode}, one completed turn, one failed turn, three redacted surfaces")


def verify(binary, root, endpoint, key, auth):
    route = {"base_url": endpoint, "credential_env": VARIABLE}
    if auth != "api-key":
        route["auth"] = auth
    entry = {
        "type": "claude",
        "unrestricted_tools": True, "working_directory": str(root),
        "environment": ["PATH"], "model": "claude-fake", "model_endpoint": route,
    }
    launcher = root / "claude"
    launcher.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{Path(__file__).resolve()}" --child "$@"\n')
    launcher.chmod(0o700)
    entry["executable"] = str(launcher)
    registry = root / "hub.json"
    registry.write_text(json.dumps({"adapters": {"routed": entry}}))
    environment = {"PATH": os.defpath, "HOME": str(root), "TMPDIR": str(root),
                   "OXAP_KEYCHAIN_SERVICE": f"oxap-verify-{uuid.uuid4()}", "NODE_ENV": "test", VARIABLE: key}
    command = [str(binary), "serve", "--stdio", "--no-save-oapx-sessions", "--config", str(registry)]
    fixture = ROOT / "go/cmd/goap/testdata/parity/claude/scenario.jsonl"
    frames = [json.loads(line) for line in fixture.read_text().splitlines()]
    opening = frames[2]
    submit = frames[4]
    with tempfile.TemporaryFile() as stderr, subprocess.Popen(
        command, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0,
    ) as hub:
        try:
            observed = exchange(hub, [{"id": 1, "op": "open", "adapter": "routed", "request": opening}],
                                lambda line: line.get("id") == 1)
            if b"session.open.response" not in observed:
                raise AssertionError(observed.replace(key.encode(), VARIABLE.encode()).decode())
            observed += exchange(hub, [{"id": 2, "op": "events", "session_id": "s1"}],
                                 lambda line: line.get("id") == 2)
            assert b'"error"' not in observed, "hub refused the event subscription"
            for index, terminal in enumerate((b"run.completed", b"run.failed"), 3):
                submit["id"] = f"submit-{index}"
                observed += exchange(hub, [{"id": index, "op": "submit", "session_id": "s1", "request": submit}],
                                     lambda line: terminal in json.dumps(line).encode())
            hub.stdin.close()
            assert hub.wait(timeout=10) == 0, f"hub exit={hub.returncode}"
        finally:
            if hub.poll() is None:
                hub.kill()
                hub.wait(timeout=10)
        stderr.seek(0)
        logs = stderr.read()
        assert key.encode() not in observed + logs, "sentinel escaped into hub output"
        assert f"native output: {VARIABLE}".encode() in observed, "normal output lacks the replacement"
        assert f"native failure: {VARIABLE}".encode() in observed, "failure lacks the replacement"
        assert f"native stderr: {VARIABLE}".encode() in logs, "stderr lacks the replacement"
        print(f"{auth}: hub exit={hub.returncode}, one completed turn, one failed turn, three redacted surfaces")

    if auth == "bearer":
        verify_http(command, environment, opening, submit, key)

    refused = [
        ({"model_provider": "anthropic"}, "model_provider"),
        ({"environment": [VARIABLE]}, VARIABLE),
        ({"environment": ["ANTHROPIC_AUTH_TOKEN="]}, "ANTHROPIC_AUTH_TOKEN"),
        ({"environment": ["ANTHROPIC_API_KEY"]}, "ANTHROPIC_API_KEY"),
        ({"type": "memory"}, "routes only claude"),
        ({"model_endpoint": None}, "must be an object"),
        ({"model_endpoint": {**route, "wire": "openai-responses"}}, "anthropic-messages"),
        ({"model_endpoint": {**route, "base_url": endpoint + "?"}}, "base_url"),
        ({"model_endpoint": {**route, "base_url": endpoint + "#"}}, "base_url"),
        ({"model_endpoint": {**route, "base_url": "https://user@localhost"}}, "base_url"),
        ({"model_endpoint": {**route, "base_url": "/gateway"}}, "base_url"),
        ({"model_endpoint": {**route, "base_url": endpoint + "%zz"}}, "base_url"),
        ({"model_endpoint": {**route, "base_url": "http://[::1]suffix"}}, "base_url"),
        ({"model_endpoint": {**route, "base_url": "http://192.0.2.1"}}, "base_url"),
        ({"model_endpoint": {**route, "credential_env": "ABSENT"}}, "ABSENT"),
        ({"model_endpoint": {**route, "auth": "invalid"}}, "auth"),
    ]
    for override, diagnostic in refused:
        registry.write_text(json.dumps({"adapters": {"routed": {**entry, **override}}}))
        result = subprocess.run(command, env=environment, input=b"", capture_output=True, timeout=10)
        assert key.encode() not in result.stderr + result.stdout, "sentinel escaped in a startup refusal"
        assert result.returncode != 0 and b'"routed"' in result.stderr, "expected a named startup refusal"
        assert diagnostic.encode() in result.stderr, f"refusal lacks {diagnostic}"
    print(f"{auth}: {len(refused)} named startup refusals with nonzero exit")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--child", action="store_true")
    args, extra = parser.parse_known_args()
    if args.child:
        child()
        return
    if extra or args.binary is None:
        parser.error("--binary is required")
    key = uuid.uuid4().hex
    with http.server.HTTPServer(("127.0.0.1", 0), Endpoint) as server:
        server.observed = []
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        try:
            for auth in ("api-key", "bearer"):
                with tempfile.TemporaryDirectory(prefix="oxap-endpoint-") as directory:
                    verify(args.binary.resolve(), Path(directory).resolve(),
                           f"http://127.0.0.1:{server.server_port}/gateway/", key, auth)
        finally:
            server.shutdown()
            worker.join()
        assert server.observed == [
            ("/gateway/messages", key, None),
            ("/gateway/messages", None, "Bearer " + key),
            ("/gateway/messages", None, "Bearer " + key),
        ], "stub upstream saw the wrong path or authentication scheme"
        print(f"PASS: stub upstream observed {len(server.observed)} requests with the expected auth schemes")


if __name__ == "__main__":
    main()
