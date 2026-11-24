from contextlib import ExitStack
import json
from pathlib import Path
import runpy
import sys
import tempfile
import time

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

    def turn(status, turn_id="turn-1"):
        return {"id": turn_id, "status": status}

    def delta(text, turn_id="turn-1"):
        notify("item/agentMessage/delta", turnId=turn_id, itemId="message-1", delta=text)

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
            result = {"thread": {"id": THREAD}, "model": "fixture",
                      "sandbox": {"type": "readOnly"}, "approvalPolicy": "never", "cwd": str(Path.cwd())}
        elif method == "turn/start":
            turn_id = "turn-2" if params.get("model") == "new-model" else "turn-1"
            result = {"turn": turn("inProgress", turn_id)}
        elif method == "turn/interrupt":
            result = {}
        elif "id" not in call:
            continue
        else:
            raise AssertionError(method)
        emit({"id": call["id"], "result": result})
        if method == "thread/resume":
            await_file(Path("replay"))
            usage(18000, 128000, 80000)
        if method == "turn/start":
            notify("turn/started", turn=turn("inProgress", turn_id))
            if turn_id == "turn-2":
                delta("model changed", turn_id)
                await_file(Path("finish-switch"))
                notify("turn/completed", turn=turn("completed", turn_id))
                continue
            usage(54000, 128000, 54000)
            usage(78000, 128000, 78000, thread="other-thread")
            usage(54000, 128000, 54000)
            delta("before compaction")
            await_file(Path("compact"))
            item = {"id": "compact-1", "type": "contextCompaction"}
            notify("item/started", turnId="turn-1", item=item)
            usage(18000, None, 80000)
            usage(18000, 128000, 80000)
            notify("item/completed", turnId="turn-1", item=item)
            delta("after compaction")
            await_file(Path("finish"))
            notify("turn/completed", turn=turn("completed"))


def await_file(path):
    deadline = time.monotonic() + 30
    while not path.exists():
        assert time.monotonic() < deadline, f"client did not release {path.name}"
        time.sleep(0.01)


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

    def event_until(hub, matches):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            for line in hub.seen:
                if matches(line.get("envelope", {})):
                    return line["envelope"]
            line = hub.lines.get(timeout=max(0.1, deadline - time.monotonic()))
            assert line is not None, "hub exited before the expected event"
            hub.seen.append(line)
        raise AssertionError("hub did not publish the expected event")

    def at_delta(hub, text):
        event_until(hub, lambda event: event.get("type") == "content.delta"
                    and event["payload"]["part"]["text"] == text)
        return [frame for frame in hub.seen if frame.get("envelope", {}).get("type") == "session.state.updated"]

    def submit(hub, **options):
        reply = hub.call("submit", session_id=SESSION, request=HUB["envelope"](
            "session.message.submit.request", {"session_id": SESSION, "delivery": "auto",
            "messages": [{"role": "user", "content": "context fixture"}], **options}))
        assert reply.get("ok"), reply
        return reply["result"]["payload"]["run_id"]

    first = connect("first")
    assert ring(opened(first)) == (None, None)
    assert first.call("events", session_id=SESSION)["ok"]
    run_id = submit(first)
    initial = at_delta(first, "before compaction")
    assert len(initial) == 1, f"expected 1 live context update before compaction; received {len(initial)}"
    assert ring(initial[0]["envelope"]["payload"]) == (54000 / 128000, None)
    (root / "compact").touch()
    updates = at_delta(first, "after compaction")
    assert len(updates) == 4, f"expected 4 live context updates before run.completed; received {len(updates)}"
    assert all("sequence" not in frame for frame in updates)
    envelopes = [frame["envelope"] for frame in updates]
    assert [event["sequence"] for event in envelopes] == [1, 2, 3, 4], envelopes
    payloads = [event["payload"] for event in envelopes]
    assert all(payload["active_run_id"] == run_id for payload in payloads), payloads
    assert [payload.get("context_usage") for payload in payloads] == [
        {"used_tokens": 54000, "window_tokens": 128000}, None, {"used_tokens": 18000},
        {"used_tokens": 18000, "window_tokens": 128000}], payloads
    assert list(map(ring, payloads)) == [(54000 / 128000, None), (None, None),
                                         (None, None), (18000 / 128000, None)]
    (root / "finish").touch()
    terminal = first.completed(SESSION)
    assert terminal["payload"]["usage"]["total_tokens"] == 80000, terminal
    assert ring(state(first)) == (18000 / 128000, None)
    first.stop()

    second = connect("reattach")
    assert ring(opened(second, reopen=True)) == (None, None)
    assert second.call("events", session_id=SESSION)["ok"]
    (root / "replay").touch()
    restored = event_until(second, lambda event: event.get("type") == "session.state.updated")
    assert restored["payload"]["status"] == "idle", restored
    assert ring(restored["payload"]) == (18000 / 128000, None)
    assert ring(state(second)) == (18000 / 128000, None)
    switched_run = submit(second, model_id="new-model")
    cleared = at_delta(second, "model changed")[-1]["envelope"]["payload"]
    assert cleared["active_run_id"] == switched_run and "context_usage" not in cleared, cleared
    assert ring(state(second)) == (None, None)
    (root / "finish-switch").touch()
    second.completed(SESSION)
    second.stop()
    print(f"PASS: live updates={len(updates)}, restored=18000/128000, cleared after model change, threshold marker absent; "
          f"hub exits={first.process.returncode},{second.process.returncode}")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--native"]:
        native()
    else:
        with tempfile.TemporaryDirectory(prefix="oxap-context-") as temp, ExitStack() as cleanup:
            verify(Path(sys.argv[1]).resolve(), Path(temp).resolve(), cleanup)
