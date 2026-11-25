import json
from pathlib import Path
import runpy
import sys
import tempfile

HUB = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
MODEL = "gpt-6.2-sol"


def native():
    def emit(message):
        print(json.dumps(message), flush=True)

    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        params = request.get("params") or {}
        if method == "initialize":
            result = {"userAgent": "fixture", "codexHome": "/fixture", "platformFamily": "unix", "platformOs": "macos"}
        elif method == "model/list":
            result = {"data": [{"id": MODEL, "model": MODEL, "isDefault": True,
                                "defaultReasoningEffort": "high",
                                "supportedReasoningEfforts": [{"reasoningEffort": level, "description": level}
                                                              for level in ("high", "ultra", "persistent")]}]}
        elif method == "config/read":
            result = {"config": {"model": MODEL}}
        elif method == "thread/start":
            with Path("efforts.jsonl").open("a") as log:
                print(json.dumps([method, params.get("config", {}).get("model_reasoning_effort")]), file=log)
            result = {"thread": {"id": "thread"}, "model": MODEL}
        elif method == "turn/start":
            with Path("efforts.jsonl").open("a") as log:
                print(json.dumps([method, params.get("effort")]), file=log)
            result = {"turn": {"id": "turn", "status": "inProgress"}}
        else:
            continue
        emit({"id": request["id"], "result": result})
        if method == "turn/start":
            for status, event in [("inProgress", "started"), ("completed", "completed")]:
                emit({"method": f"turn/{event}", "params": {
                    "threadId": "thread", "turn": {"id": "turn", "status": status}}})


def verify(binary, root):
    (root / "home").mkdir()
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": {
        "type": "codex", "executable": str(Path(sys.executable).resolve()),
        "args": [str(Path(__file__).resolve()), "--native"],
        "model": MODEL, "working_directory": str(root), "environment": ["HOME", "PATH"],
    }}}))
    hub = HUB["Hub"]([str(binary), "serve", "--stdio", "--config", str(config),
                      "--no-save-oapx-sessions", "--session-history", str(root / "sessions.jsonl")],
                     root, "hub")

    def request(op, kind, session, **payload):
        envelope = HUB["envelope"](kind + ".request", {"session_id": session, **payload})
        routing = {"adapter": "codex"} if op == "open" else {"session_id": session}
        return hub.call(op, request=envelope, **routing)

    def payload(reply):
        assert reply["ok"], reply
        return reply["result"]["payload"]

    def open_session(session, level):
        return request("open", "session.open", session, reasoning_level=level)

    def update(level):
        return request("settings", "session.settings.update", "live", reasoning_level=level)

    try:
        opened = open_session("open", "ultra")
        print("open:", opened.get("error", {}).get("code", "ok"), flush=True)
        assert payload(open_session("live", "high"))["reasoning_level"] == "high"
        updated = update("ultra")
        print("update:", updated.get("error", {}).get("code", "ok"), flush=True)
        assert payload(opened)["reasoning_level"] == "ultra"
        assert payload(updated)["reasoning_level"] == "ultra"
        assert payload(updated)["previous_reasoning_level"] == "high"
        assert payload(hub.call("state", session_id="live"))["reasoning_level"] == "ultra"
        for reply in (open_session("invalid", "persistent"), update("persistent")):
            assert not reply["ok"] and reply["error"]["code"] == "schema_invalid", reply
        assert hub.call("events", session_id="live")["ok"]
        payload(request("submit", "session.message.submit", "live", delivery="auto",
                        messages=[{"role": "user", "content": "hello"}]))
        hub.completed("live")
        calls = [json.loads(line) for line in (root / "efforts.jsonl").read_text().splitlines()]
        assert calls == [["thread/start", "ultra"], ["thread/start", "high"], ["turn/start", "ultra"]], calls
        hub.stop()
        print(f"PASS: 2 ultra settings, 2 persistent refusals, {len(calls)} native calls, hub exit={hub.process.returncode}")
    finally:
        hub.cleanup()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--native"]:
        native()
    else:
        with tempfile.TemporaryDirectory(prefix="oap-codex-ultra-") as temp:
            verify(Path(sys.argv[1]).resolve(), Path(temp).resolve())
