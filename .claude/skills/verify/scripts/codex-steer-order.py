import argparse
import json
from pathlib import Path
import runpy
import subprocess
import sys

HUB = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
SCOPE = {"threadId": "fixture-thread", "turnId": "fixture-turn"}


def fake_codex():
    def emit(frame):
        print(json.dumps(frame), flush=True)

    def notify(method, **params):
        emit({"method": method, "params": {**SCOPE, **params}})

    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        if method == "initialized":
            continue
        if method == "initialize":
            result = {"userAgent": "fixture", "codexHome": "/fixture",
                      "platformFamily": "unix", "platformOs": "macos"}
        elif method == "config/read":
            result = {"config": {"model": "fixture-model"}}
        elif method == "model/list":
            result = {"data": [], "nextCursor": None}
        elif method == "thread/start":
            result = {"thread": {"id": SCOPE["threadId"]}, "model": "fixture-model"}
        elif method == "turn/start":
            result = {"turn": {"id": SCOPE["turnId"], "status": "inProgress"}}
        elif method == "turn/steer":
            assert request["params"]["expectedTurnId"] == SCOPE["turnId"], request
            notify("item/agentMessage/delta", itemId="assistant", delta="Already streaming.")
            result = {"turnId": SCOPE["turnId"]}
        else:
            raise AssertionError(method)
        emit({"id": request["id"], "result": result})
        if method == "turn/start":
            notify("turn/started", turn=result["turn"])
        if method == "turn/steer":
            notify("item/completed", item={"type": "userMessage", "id": "guidance",
                                           "clientId": request["params"]["clientUserMessageId"]})
            notify("turn/completed", turn={"id": SCOPE["turnId"], "status": "completed"})


def verify(binary, output):
    output.mkdir()
    (output / "home").mkdir()
    config = output / "registry.json"
    config.write_text(json.dumps({"adapters": {"codex": {
        "type": "codex", "executable": str(Path(sys.executable).resolve()),
        "args": [str(Path(__file__).resolve()), "--fixture"], "working_directory": str(output),
        "model": "fixture-model", "environment": ["HOME", "PATH"],
    }}}))
    endpoint = HUB["Hub"]([str(binary), "serve", "agent", "--backend", "codex",
                           "--config", str(config)], output, "endpoint")
    revision = None

    def read_until(predicate):
        while True:
            frame = endpoint.lines.get(timeout=15)
            assert frame is not None, "endpoint exited"
            endpoint.seen.append(frame)
            if predicate(frame):
                return frame

    def call(kind, payload):
        frame = HUB["envelope"](kind, {"session_id": "s1", **payload})
        if kind in ("protocol.initialize.request", "capabilities.request"):
            del frame["session_id"]
            del frame["payload"]["session_id"]
        if revision:
            frame["capability_revision"] = revision
        endpoint.seen.append(frame)
        endpoint.process.stdin.write(json.dumps(frame) + "\n")
        endpoint.process.stdin.flush()
        response = read_until(lambda item: item.get("in_reply_to") == frame["id"])
        assert response["type"] == kind.removesuffix(".request") + ".response", response
        return response

    try:
        call("protocol.initialize.request", {"protocol_versions": ["0.1"],
                                              "profiles": ["open-agent-protocol.agent-control-core"],
                                              "participant": {"id": "user"}})
        revision = call("capabilities.request", {})["capability_revision"]
        call("session.open.request", {})
        message = {"role": "user", "content": "Start the turn."}
        start = call("session.message.submit.request", {"delivery": "auto", "messages": [message]})
        run_id = start["payload"]["run_id"]
        prior = read_until(lambda item: item["type"] == "run.started")
        steer = call("session.message.submit.request", {"delivery": "steer", "messages": [message],
                                                        "target_run_id": run_id})
        delta = read_until(lambda item: item["type"] == "content.delta")
        applied = read_until(lambda item: item["type"] == "run.steer.applied")
        terminal = read_until(lambda item: item["type"] == "run.completed")
        endpoint.stop()
        trace = output / "trace.json"
        trace.write_text(json.dumps(endpoint.seen, indent=2) + "\n")
        validation = subprocess.run([str(binary), "validate", str(trace)], capture_output=True,
                                    text=True, timeout=15)
        (output / "validation.log").write_text(validation.stdout + validation.stderr)
        print(f"validator exit={validation.returncode}, target={steer['payload']['target_sequence']}, "
              f"previous={prior['sequence']}, delta={delta['sequence']}, "
              f"applied={applied['sequence']}, terminal={terminal['sequence']}")
        assert steer["payload"]["target_sequence"] == prior["sequence"], steer
        assert [prior["sequence"], delta["sequence"], applied["sequence"], terminal["sequence"]] == [1, 2, 3, 4]
        validation.check_returncode()
    finally:
        endpoint.cleanup()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fixture"]:
        fake_codex()
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("binary", type=Path)
        parser.add_argument("output", type=Path)
        args = parser.parse_args()
        verify(args.binary.resolve(), args.output.resolve())
