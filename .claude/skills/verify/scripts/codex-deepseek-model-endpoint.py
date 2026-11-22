import argparse
import http.server
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import threading
import urllib.request
import uuid


CLAUDE = runpy.run_path(str(Path(__file__).with_name("claude-model-endpoint.py")))
ROOT = CLAUDE["ROOT"]
Endpoint = CLAUDE["Endpoint"]
exchange = CLAUDE["exchange"]
VARIABLE = "ROUTE_FIXTURE_VALUE"


def upstream(kind, fixture, args):
    assert VARIABLE not in os.environ
    if kind == "codex":
        key = os.environ["OAPX_PROVIDER_API_KEY"]
        settings = dict(value.split("=", 1) for flag, value in zip(args, args[1:]) if flag == "-c")
        assert args[0] == "--fixture-option"
        assert settings["model_provider"] == '"oapx-custom"'
        prefix = "model_providers.oapx-custom."
        assert settings[prefix + "env_key"] == '"OAPX_PROVIDER_API_KEY"'
        assert settings[prefix + "wire_api"] == '"responses"'
        base = json.loads(settings[prefix + "base_url"])
        path = "responses"
        headers = {"Authorization": "Bearer " + key}
    else:
        key = os.environ["DEEPSEEK_API_KEY"]
        base = os.environ["DEEPSEEK_BASE_URL"]
        assert os.path.isdir(os.environ["DSH_HOME"])
        path = "v1/messages"
        headers = {"X-Api-Key": key}
    assert base.endswith("/gateway/"), "the harness base URL was rewritten"
    assert all(key not in arg for arg in args), "sentinel reached child argv"
    request = urllib.request.Request(base + path, data=b"{}", headers=headers)
    with urllib.request.urlopen(request, timeout=5) as response:
        assert response.status == 204
    os.write(2, b"native stderr: " + key.encode() + b"\n")
    os.execv("/bin/sh", ["/bin/sh", fixture])


def verify(binary, root, base, kind, key):
    fixture = ROOT / "go/cmd/goap/testdata/parity" / kind
    child = root / "native.sh"
    script = (fixture / "child.sh").read_text()
    if kind == "codex":
        script = script.replace('"delta":"fixture-"', '''"delta":"'"$OAPX_PROVIDER_API_KEY"'"''', 1)
        wire, destination = "openai-responses", "OAPX_PROVIDER_API_KEY"
    else:
        script = script.replace('"text":"Hel"', '''"text":"'"$DEEPSEEK_API_KEY"'"''', 1)
        wire, destination = "anthropic-messages", "DEEPSEEK_BASE_URL"
    child.write_text(script)
    launcher = root / "launcher"
    launcher.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{Path(__file__).resolve()}" --child {kind} "{child}" "$@"\n')
    launcher.chmod(0o700)
    entry = json.loads((fixture / "registry.json").read_text().replace("@DIR@", str(root)))["adapters"][kind]
    route = {"base_url": base, "credential_env": VARIABLE}
    if kind == "deepseek":
        route["wire"] = wire
    entry.update(executable=str(launcher), model_endpoint=route)
    home = root / "home"
    home.mkdir()
    dsh = root / "dsh"
    dsh.mkdir()
    if kind == "deepseek":
        entry["environment"].extend(["HOME", "DSH_HOME=dsh"])
    else:
        entry["args"] = ["--fixture-option"]
    environment = {"PATH": os.defpath, "HOME": str(home), "NODE_ENV": "test", VARIABLE: key}
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"routed": entry}}))
    command = [str(binary), "serve", "--stdio", "--no-save-oapx-sessions", "--config", str(config)]
    frames = [json.loads(line) for line in (fixture / "scenario.jsonl").read_text().splitlines()]
    open_request, submit = frames[2], frames[5 if kind == "codex" else 4]
    with tempfile.TemporaryFile() as stderr, subprocess.Popen(
        command, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, bufsize=0,
    ) as hub:
        try:
            observed = exchange(hub, [{"id": 1, "op": "open", "adapter": "routed", "request": open_request}],
                                lambda line: line.get("id") == 1)
            assert b"session.open.response" in observed, f"{kind} did not open a routed session"
            observed += exchange(hub, [{"id": 2, "op": "events", "session_id": "s1"}],
                                 lambda line: line.get("id") == 2)
            observed += exchange(hub, [{"id": 3, "op": "submit", "session_id": "s1", "request": submit}],
                                 lambda line: b"run.completed" in json.dumps(line).encode())
            hub.stdin.close()
            assert hub.wait(timeout=10) == 0, f"hub exit={hub.returncode}"
        finally:
            if hub.poll() is None:
                hub.kill()
                hub.wait(timeout=10)
        stderr.seek(0)
        logs = stderr.read()
        assert key.encode() not in observed + logs, "sentinel escaped into hub output"
        assert any(b"content.delta" in line and VARIABLE.encode() in line for line in observed.splitlines()), "normal output lacks the replacement"
        assert f"native stderr: {VARIABLE}".encode() in logs, "stderr lacks the replacement"
        print(f"{kind}: hub exit={hub.returncode}, one completed turn, two redacted surfaces")

    refused = [
        ({"model_provider": "openai"}, "two providers"),
        ({"model_endpoint": {**route, "auth": "bearer"}}, "auth"),
        ({"model_endpoint": {**route, "wire": "openai-completions"}}, wire),
        ({"environment": [VARIABLE]}, VARIABLE),
        ({"environment": [destination]}, destination),
    ]
    if kind == "codex":
        refused.append(({"endpoint": "unix:///unavailable", "executable": "", "args": []}, "model_endpoint"))
    else:
        refused.extend([
            ({"args": ["--patch", "overlay.yaml"]}, "--patch"),
            ({"args": ["--patch=overlay.yaml"]}, "--patch"),
        ])
        (dsh / ".credentials.yaml").touch()
        refused.append(({}, ".credentials.yaml"))
        (home / ".dsh").mkdir()
        (home / ".dsh" / ".credentials.yaml").touch()
        for dsh_override in ([], ["DSH_HOME=\u00a0"], ["DSH_HOME=~/.dsh"]):
            refused.append(({"environment": ["HOME", "PATH", *dsh_override]}, ".credentials.yaml"))
        cwd = root / "real" / "cwd"
        cwd.mkdir(parents=True)
        alias = root / "alias"
        alias.symlink_to(cwd, target_is_directory=True)
        managed = cwd.parent / "managed"
        managed.mkdir()
        (managed / ".credentials.yaml").touch()
        refused.append(({"working_directory": str(alias), "environment": ["DSH_HOME=../managed"]}, ".credentials.yaml"))
    for override, diagnostic in refused:
        config.write_text(json.dumps({"adapters": {"routed": {**entry, **override}}}))
        result = subprocess.run(command, env=environment, input=b"", capture_output=True, timeout=10)
        assert key.encode() not in result.stderr + result.stdout, "sentinel escaped in startup refusal"
        assert result.returncode != 0 and b'"routed"' in result.stderr, f"{kind} expected a named startup refusal"
        assert diagnostic.encode() in result.stderr, f"{kind} refusal lacks {diagnostic}"
    print(f"{kind}: {len(refused)} named startup refusals with nonzero exit")


def main():
    if sys.argv[1:2] == ["--child"]:
        upstream(sys.argv[2], sys.argv[3], sys.argv[4:])
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary.resolve()
    key = uuid.uuid4().hex
    with http.server.HTTPServer(("127.0.0.1", 0), Endpoint) as server:
        server.observed = []
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        try:
            for kind in ("codex", "deepseek"):
                with tempfile.TemporaryDirectory(prefix="oxap-route-") as directory:
                    verify(binary, Path(directory).resolve(),
                           f"http://127.0.0.1:{server.server_port}/gateway/", kind, key)
        finally:
            server.shutdown()
            worker.join()
        assert server.observed == [
            ("/gateway/responses", None, "Bearer " + key),
            ("/gateway/v1/messages", key, None),
        ], "stub upstream saw the wrong paths or authentication"
        print(f"PASS: stub upstream observed {len(server.observed)} requests with expected auth")


if __name__ == "__main__":
    main()
