import argparse
import http.server
import json
import os
from pathlib import Path
import runpy
import shlex
import subprocess
import sys
import tempfile
import threading
import urllib.request
import uuid


SHARED = runpy.run_path(str(Path(__file__).with_name("claude-model-endpoint.py")))
VARIABLE = SHARED["VARIABLE"]
exchange = SHARED["exchange"]
MODEL = "gateway-model"
SIBLING = "OPENAI_API_KEY_2"


class Endpoint(SHARED["Endpoint"]):
    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.payloads.append(payload)
        super().do_POST()


def child():
    def send(**frame):
        print(json.dumps({"jsonrpc": "2.0", **frame}), flush=True)

    def event(kind, seq, **payload):
        send(method="event", params={"type": kind, "session_id": "native", "seq": seq,
                                     "payload": payload})

    send(method="event", params={"type": "gateway.ready", "payload": {
        "skin": {}, "change_events": True, "replay_epoch": "e3b0c44298fc1c149afbf4c8996fb924",
    }})
    opening = json.loads(sys.stdin.readline())
    assert opening["method"] == "session.create"
    assert opening["params"] == {"model": MODEL, "provider": "openai-api"}, "incorrect session.create route"
    assert VARIABLE not in os.environ and SIBLING not in os.environ
    assert os.environ["HERMES_INFERENCE_PROVIDER"] == "decoy-provider"
    root = Path(os.environ["HOME"])
    home = Path(os.environ.get("HERMES_HOME", root / ".hermes"))
    base, token, path = os.environ.get("OPENAI_BASE_URL"), os.environ.get("OPENAI_API_KEY"), "/responses"
    if home == root / ".hermes":
        config = json.loads((home / "config.yaml").read_text())["model"]
        dotenv = dict(line.split("=", 1) for line in (home / ".env").read_text().splitlines())
        pool = json.loads((home / "auth.json").read_text())["credential_pool"]["openai-api"][0]
        base = config.get("base_url", dotenv["OPENAI_BASE_URL"])
        token = pool.get("access_token", dotenv["OPENAI_API_KEY"])
        path = "/chat/completions" if config["api_mode"] == "chat_completions" else path
    else:
        assert home.parent == root / ".oxap" / "hermes", "Hermes home is outside hub state"
        assert not list(home.iterdir()), "Hermes home was not fresh"
        (home / "used").touch()
    assert base and token, "missing Hermes route"
    send(id=opening["id"], result={"session_id": "native", "stored_session_id": "stored",
                                   "message_count": 0, "info": {}})

    assert all(token not in arg for arg in sys.argv)
    os.write(2, b"native stderr: " + token.encode() + b"\n")
    for index, line in enumerate(sys.stdin):
        prompt = json.loads(line)
        assert prompt["method"] == "prompt.submit"
        request = urllib.request.Request(base + path,
            data=json.dumps({"model": MODEL, "input": prompt["params"]["text"]}).encode(),
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=5) as reply:
            assert reply.status == 204
        send(id=prompt["id"], result={"status": "streaming"})
        event("message.start", 3 * index + 1)
        event("message.delta", 3 * index + 2, text="native output: " + token)
        if index == 0:
            event("message.complete", 3, text="done", status="complete")
        else:
            event("message.complete", 3 * index + 3, error="native failure: " + token, status="error")


def envelope(kind, **payload):
    return {"protocol": "open-agent-protocol", "version": "0.1",
            "profile": "open-agent-protocol.agent-control-core", "type": kind,
            "id": kind, "session_id": "s1", "payload": {"session_id": "s1", **payload}}


def verify_turns(command, env, sentinel, source):
    with tempfile.TemporaryFile() as stderr, subprocess.Popen(
        command, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0,
    ) as hub:
        def request(id_, op, **fields):
            return exchange(hub, [{"id": id_, "op": op, **fields}], lambda line: line.get("id") == id_)

        try:
            observed = request(1, "open", adapter="routed", request=envelope("session.open.request"))
            assert b"session.open.response" in observed, "Hermes did not open a routed session"
            observed += request(2, "events", session_id="s1")
            for id_, terminal in ((3, "run.completed"), (4, "run.failed")):
                submit = envelope("session.message.submit.request", delivery="auto",
                                  messages=[{"role": "user", "content": "fixture input"}])
                submit["id"] = f"submit-{id_}"
                observed += exchange(hub, [{"id": id_, "op": "submit", "session_id": "s1", "request": submit}],
                                     lambda line: line.get("envelope", {}).get("type") == terminal)
            hub.stdin.close()
            assert hub.wait(timeout=10) == 0, "Hermes hub failed to exit cleanly"
        finally:
            if hub.poll() is None:
                hub.kill()
                hub.wait(timeout=10)
        stderr.seek(0)
        logs = stderr.read()
        assert sentinel.encode() not in observed + logs, "sentinel escaped into hub output"
        for label, surface in (("native output", observed), ("native failure", observed), ("native stderr", logs)):
            assert f"{label}: {source}".encode() in surface, f"missing redaction of {label}"


def verify(binary, root, real, decoy):
    base = f"http://127.0.0.1:{real.server_port}/gateway/v1"
    decoy_url = f"http://127.0.0.1:{decoy.server_port}/decoy/v1"
    default_home = root / ".hermes"
    default_home.mkdir()
    other = uuid.uuid4().hex
    (default_home / "config.yaml").write_text(json.dumps({"model": {
        "provider": "openai-api", "base_url": decoy_url, "api_mode": "chat_completions",
    }}))
    (default_home / ".env").write_text(f"OPENAI_BASE_URL={decoy_url}\nOPENAI_API_KEY={other}\n")
    (default_home / "auth.json").write_text(json.dumps({"credential_pool": {"openai-api": [{
        "id": "decoy", "source": "manual", "label": "decoy", "auth_type": "api_key",
        "access_token": other, "base_url": decoy_url,
    }]}}))
    launcher = root / "launcher"
    launcher.write_text("#!/bin/sh\nexec " + shlex.join([sys.executable, str(Path(__file__).resolve()), "--child"]) + "\n")
    launcher.chmod(0o700)
    config = root / "hub.json"
    command = [str(binary), "serve", "--stdio", "--no-save-oapx-sessions", "--config", str(config)]
    sentinel = uuid.uuid4().hex
    env = {"PATH": os.defpath, "HOME": str(root), "TMPDIR": str(root), "NODE_ENV": "test",
           "HERMES_HOME": str(default_home), "HERMES_INFERENCE_PROVIDER": "decoy-provider",
           VARIABLE: sentinel, SIBLING: other}
    route = {"base_url": base, "credential_env": VARIABLE, "wire": "openai-responses"}
    entry = {"type": "hermes", "executable": str(launcher), "working_directory": str(root),
             "model": MODEL, "environment": ["HOME", "HERMES_INFERENCE_PROVIDER"],
             "model_endpoint": route}

    def configure(overrides):
        config.write_text(json.dumps({"adapters": {"routed": {**entry, **overrides}}}))

    configure({})
    verify_turns(command, env, sentinel, VARIABLE)
    configure({"model_endpoint": {"base_url": base, "credential_env": SIBLING}})
    verify_turns(command, {**env, SIBLING: sentinel}, sentinel, SIBLING)
    assert real.observed == [("/gateway/v1/responses", None, "Bearer " + sentinel)] * 4, "incorrect upstream route or auth"
    assert real.payloads == [{"model": MODEL, "input": "fixture input"}] * 4, "wrong Responses payload"
    assert not decoy.observed, "decoy saw a request"
    assert len(list((root / ".oxap" / "hermes").glob("*/used"))) == 2, "routed Hermes homes were not distinct"
    print("Hermes: two fresh homes, four Responses requests, zero decoy requests, three redacted surfaces per hub")

    refused = [
        ({"environment": ["HERMES_HOME"]}, "HERMES_HOME"),
        ({"environment": ["OPENAI_BASE_URL"]}, "OPENAI_BASE_URL"),
        ({"environment": ["OPENAI_API_KEY"]}, "OPENAI_API_KEY"),
        ({"environment": [VARIABLE]}, VARIABLE),
        ({"environment": [SIBLING]}, SIBLING),
        ({"environment": ["OPENAI_API_KEY_19=unused"]}, "OPENAI_API_KEY_19"),
        ({"model_endpoint": {**route, "wire": "openai-completions"}}, "openai-responses"),
    ]
    refused.extend(({"model_endpoint": {**route, "base_url": url}}, "reclassifies") for url in (
        "https://API.ANTHROPIC.COM./v1",
        "https://api.actual.inc/v1",
        base + "/ANTHROPIC///",
        base + "/anthropic/v1",
        "https://api.kimi.com/coding/v1",
        "https://bedrock-runtime.us-east-1.amazonaws.com/v1",
    ))
    for overrides, diagnostic in refused:
        configure(overrides)
        result = subprocess.run(command, env=env, capture_output=True, input=b"", timeout=10)
        assert all(value.encode() not in result.stdout + result.stderr for value in (sentinel, other)), "credential escaped into startup refusal"
        assert result.returncode != 0 and b'"routed"' in result.stderr, "expected a named startup refusal"
        assert diagnostic.encode() in result.stderr, f"refusal lacks {diagnostic}"
    assert len(real.observed) == 4 and not decoy.observed, "startup refusal contacted an endpoint"
    print(f"Hermes: {len(refused)} named startup refusals with nonzero exit")


def main():
    if sys.argv[1:] == ["--child"]:
        child()
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary.resolve()
    with http.server.HTTPServer(("127.0.0.1", 0), Endpoint) as real, \
            http.server.HTTPServer(("127.0.0.1", 0), Endpoint) as decoy, \
            tempfile.TemporaryDirectory(prefix="hermes-route-") as directory:
        workers = []
        for server in (real, decoy):
            server.observed, server.payloads = [], []
            worker = threading.Thread(target=server.serve_forever)
            worker.start()
            workers.append(worker)
        try:
            verify(binary, Path(directory).resolve(), real, decoy)
        finally:
            for server, worker in zip((real, decoy), workers):
                server.shutdown()
                worker.join()


if __name__ == "__main__":
    main()
