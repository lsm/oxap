from contextlib import ExitStack
import json
from pathlib import Path
import runpy
import sys
import tempfile

HUB = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
SESSION = "context-session"
THREAD = "fixture-thread"


def native():
    def emit(value):
        print(json.dumps(value), flush=True)

    def notify(method, **params):
        emit({"method": method, "params": {"threadId": THREAD, **params}})

    def usage(used, window, total, thread=THREAD):
        def breakdown(tokens):
            return {"totalTokens": tokens, "inputTokens": tokens - 10, "outputTokens": 10,
                    "cachedInputTokens": 300, "reasoningOutputTokens": 0}

        emit({"method": "thread/tokenUsage/updated", "params": {
            "threadId": thread, "turnId": "turn-1", "tokenUsage": {
                "last": breakdown(used), "total": breakdown(total), "modelContextWindow": window}}})

    def turn(status):
        return {"id": "turn-1", "status": status}

    def delta(text):
        notify("item/agentMessage/delta", turnId="turn-1", itemId="message-1", delta=text)

    for line in sys.stdin:
        call = json.loads(line)
        method = call["method"]
        params = call.get("params") or {}
        if method == "initialize":
            result = {"userAgent": "fixture", "codexHome": "/fixture", "platformFamily": "unix", "platformOs": "macos"}
        elif method == "model/list":
            result = {"data": [{"id": "fixture", "model": "fixture", "isDefault": True}]}
        elif method == "config/read":
            result = {"config": {}}
        elif method in ("thread/start", "thread/resume"):
            if method == "thread/resume":
                assert params["threadId"] == THREAD, params
            result = {"thread": {"id": THREAD}, "model": "fixture"}
        elif method == "turn/start":
            result = {"turn": turn("inProgress")}
        elif method == "turn/interrupt":
            result = {}
        elif "id" not in call:
            continue
        else:
            raise AssertionError(method)
        if method == "thread/resume":
            usage(18000, 128000, 80000)
        emit({"id": call["id"], "result": result})
        if method == "turn/start":
            notify("turn/started", turn=turn("inProgress"))
            usage(54000, 128000, 54000)
            usage(78000, 128000, 78000, thread="other-thread")
            usage(54000, 128000, 54000)
            delta("before compaction")
            notify("item/completed", turnId="turn-1", item={"id": "compact-1", "type": "contextCompaction"})
            usage(18000, None, 80000)
            usage(18000, 128000, 80000)
            delta("after compaction")
            notify("turn/completed", turn=turn("completed"))


def verify(binary, root, cleanup):
    (root / "home").mkdir()
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": {"type": "codex",
        "executable": sys.executable, "args": [str(Path(__file__).resolve()), "--native"],
        "working_directory": str(root), "sandbox": "read-only", "approval_policy": "never",
        "environment": ["HOME", "PATH"]}}}))
    command = [str(binary), "serve", "--stdio", "--config", str(config),
               "--no-save-oapx-sessions", "--session-history", str(root / "sessions.jsonl")]

    def connect(label):
        hub = HUB["Hub"](command, root, label)
        cleanup.callback(hub.cleanup)
        return hub

    def opened(hub, reopen=False):
        reply = hub.call("open", adapter="codex", request=HUB["envelope"](
            "session.open.request", {"session_id": SESSION, "reopen": reopen}))
        assert reply.get("ok"), reply
        return reply["result"]["payload"]

    def state(hub):
        reply = hub.call("state", session_id=SESSION)
        assert reply.get("ok"), reply
        return reply["result"]["payload"]

    def ring(payload):
        sample = payload.get("context_usage", {})
        window = sample.get("window_tokens")
        if window is None:
            return None, None
        threshold = sample.get("compaction_threshold_tokens")
        return sample["used_tokens"] / window, None if threshold is None else threshold / window

    first = connect("first")
    assert ring(opened(first)) == (None, None)
    assert first.call("events", session_id=SESSION)["ok"]
    reply = first.call("submit", session_id=SESSION, request=HUB["envelope"](
        "session.message.submit.request", {"session_id": SESSION, "delivery": "auto",
        "messages": [{"role": "user", "content": "context fixture"}]}))
    assert reply.get("ok"), reply
    terminal = first.completed(SESSION)
    updates = [frame for frame in first.seen if frame.get("envelope", {}).get("type") == "session.state.updated"]
    assert len(updates) == 4, f"expected 4 live context updates before run.completed; received {len(updates)}"
    assert all("sequence" not in frame for frame in updates)
    envelopes = [frame["envelope"] for frame in updates]
    assert [event["sequence"] for event in envelopes] == [1, 2, 3, 4], envelopes
    payloads = [event["payload"] for event in envelopes]
    assert all(payload["active_run_id"] == terminal["run_id"] for payload in payloads), payloads
    assert [payload.get("context_usage") for payload in payloads] == [
        {"used_tokens": 54000, "window_tokens": 128000}, None, {"used_tokens": 18000},
        {"used_tokens": 18000, "window_tokens": 128000}], payloads
    assert list(map(ring, payloads)) == [(54000 / 128000, None), (None, None),
                                         (None, None), (18000 / 128000, None)]
    assert terminal["payload"]["usage"]["total_tokens"] == 80000, terminal
    assert ring(state(first)) == (18000 / 128000, None)
    first.stop()

    second = connect("reattach")
    assert ring(opened(second, reopen=True)) == (18000 / 128000, None)
    assert ring(state(second)) == (18000 / 128000, None)
    second.stop()
    print(f"PASS: live updates={len(updates)}, retained=18000/128000, threshold marker absent; "
          f"hub exits={first.process.returncode},{second.process.returncode}")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--native"]:
        native()
    else:
        with tempfile.TemporaryDirectory(prefix="oxap-context-") as temp, ExitStack() as cleanup:
            verify(Path(sys.argv[1]).resolve(), Path(temp).resolve(), cleanup)
