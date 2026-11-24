import argparse
import json
from pathlib import Path
import runpy
import sys
import tempfile

HUB = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
EXTENSION = "com.openai.codex.item"
SESSION = "s1"
PARENT = "parent-thread"
CHILD = "child-thread"
TURN = "parent-turn"


def activity(item_id, kind, model=None, effort=None):
    return {"type": "subAgentActivity", "id": item_id, "kind": kind, "agentThreadId": CHILD,
            "agentPath": "/root/worker", "model": model, "reasoningEffort": effort}


def collab(item_id, tool, status, receivers=(), states=None, prompt=None):
    return {"type": "collabAgentToolCall", "id": item_id, "tool": tool, "status": status,
            "senderThreadId": PARENT, "receiverThreadIds": list(receivers), "agentsStates": states or {},
            "prompt": prompt, "model": None, "reasoningEffort": None}


STARTED = activity("spawn-item", "started", "worker-model", "medium")
COMPLETED = activity("completed-item", "completed")
WAITING = collab("wait-item", "wait", "inProgress")
WAITED = collab("wait-item", "wait", "completed")
SENDING = collab("send-item", "sendInput", "inProgress", [CHILD], prompt="Check the result.")
SENT = collab("send-item", "sendInput", "completed", [CHILD],
              {CHILD: {"status": "completed", "message": "The check passed."}}, "Check the result.")
CLOSING = collab("close-item", "closeAgent", "inProgress", [CHILD])
CLOSE_FAILED = collab("close-item", "closeAgent", "failed", [CHILD],
                      {CHILD: {"status": "notFound", "message": None}})
WAIT_INTERRUPTED = collab("wait-interrupted-item", "wait", "interrupted")
WAIT_AGAIN = collab("wait-interrupted-item", "wait", "inProgress")


def native():
    def emit(value):
        print(json.dumps(value), flush=True)

    def item(event, value, thread=PARENT, turn=TURN):
        timing = {"startedAtMs": 1}
        if event == "completed":
            timing["completedAtMs"] = 2
        emit({"method": f"item/{event}", "params": {
            "threadId": thread, "turnId": turn, "item": value, **timing}})

    def turn(event, status):
        emit({"method": f"turn/{event}", "params": {
            "threadId": PARENT, "turn": {"id": TURN, "status": status}}})

    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        if method == "initialize":
            result = {"userAgent": "fixture", "codexHome": "/fixture",
                      "platformFamily": "unix", "platformOs": "linux"}
        elif method == "model/list":
            result = {"data": [], "nextCursor": None}
        elif method == "thread/start":
            result = {"thread": {"id": PARENT}}
        elif method == "turn/start":
            result = {"turn": {"id": TURN, "status": "inProgress"}}
        else:
            continue
        emit({"id": request["id"], "result": result})
        if method != "turn/start":
            continue
        turn("started", "inProgress")
        for event in ("started", "completed"):
            item(event, activity("foreign-thread-item", "completed"), thread=CHILD)
            item(event, activity("foreign-turn-item", "completed"), turn="other-turn")
            item(event, STARTED)
        item("started", WAITING)
        item("started", COMPLETED)
        item("completed", COMPLETED)
        item("completed", WAITED)
        for start, end in [(SENDING, SENT), (CLOSING, CLOSE_FAILED), (WAIT_AGAIN, WAIT_INTERRUPTED)]:
            item("started", start)
            item("completed", end)
        turn("completed", "completed")


def replay(binary, root, output):
    (root / "home").mkdir()
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": {
        "type": "codex", "executable": str(Path(sys.executable).resolve()),
        "args": [str(Path(__file__).resolve()), "--native"], "working_directory": str(root),
        "model": "fixture", "approval_policy": "never", "sandbox": "read-only",
        "environment": []}}}))
    command = [str(binary), "serve", "--stdio", "--config", str(config),
               "--no-save-oapx-sessions", "--session-history", str(root / "sessions.jsonl")]
    hub = HUB["Hub"](command, root, "hub")
    try:
        opened = hub.call("open", adapter="codex", request=HUB["envelope"](
            "session.open.request", {"session_id": SESSION}))
        assert opened["ok"], opened
        assert hub.call("events", session_id=SESSION)["ok"]
        submitted = hub.call("submit", session_id=SESSION, request=HUB["envelope"](
            "session.message.submit.request", {"session_id": SESSION, "delivery": "auto",
            "messages": [{"role": "user", "content": "Delegate a check and wait for the result."}]}))
        assert submitted["ok"], submitted
        terminal = hub.completed(SESSION)
        hub.stop()
        print(f"hub exit={hub.process.returncode}", flush=True)
        return [frame["envelope"] for frame in hub.seen if "envelope" in frame], terminal
    finally:
        hub.cleanup()
        if output:
            (output / "hub-stderr.txt").write_text((root / "hub-stderr.txt").read_text())


def verify(events, terminal):
    run = terminal["run_id"]
    assert [event["run_id"] for event in events if event["type"] == "run.started"] == [run]
    actions = [event for event in events if event["type"].startswith("action.call.")]
    print(f"run.completed=1 action events={len(actions)}", flush=True)
    assert len(actions) == 18, "the parent run did not publish all six native items"

    by_id = {}
    for event in actions:
        assert event["run_id"] == event["payload"]["run_id"] == run, event
        assert event["session_id"] == event["payload"]["session_id"] == SESSION, event
        item = event["extensions"][EXTENSION]
        by_id.setdefault(item["id"], []).append((event, item))

    assert list(by_id) == [STARTED["id"], WAITING["id"], COMPLETED["id"],
                           SENDING["id"], CLOSING["id"], WAIT_AGAIN["id"]]
    call_ids = set()
    for start, end, name, outcome in [
        (STARTED, STARTED, "codex.subagent.activity", "completed"),
        (WAITING, WAITED, "codex.collab.wait", "completed"),
        (COMPLETED, COMPLETED, "codex.subagent.activity", "completed"),
        (SENDING, SENT, "codex.collab.sendInput", "completed"),
        (CLOSING, CLOSE_FAILED, "codex.collab.closeAgent", "failed"),
        (WAIT_AGAIN, WAIT_INTERRUPTED, "codex.collab.wait", "cancelled"),
    ]:
        frames = by_id[start["id"]]
        assert [item for _, item in frames] == [start, start, end], frames
        assert [event["type"] for event, _ in frames] == [
            f"action.call.{phase}" for phase in ("requested", "started", outcome)], frames
        call_id = frames[0][0]["tool_call_id"]
        assert call_id not in call_ids and call_id != start["id"], frames
        call_ids.add(call_id)
        for event, _ in frames:
            assert event["tool_call_id"] == event["payload"]["tool_call_id"] == call_id, frames
            assert event["payload"]["name"] == name, frames
    print(f"PASS: {len(call_ids)} action lifecycles with native item and child metadata")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--native"]:
        native()
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("binary", type=Path)
        parser.add_argument("--output-dir", type=Path)
        args = parser.parse_args()
        if args.output_dir:
            args.output_dir.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="oap-codex-subagent-") as temporary:
            events, terminal = replay(args.binary.resolve(), Path(temporary), args.output_dir)
            if args.output_dir:
                (args.output_dir / "events.json").write_text(json.dumps([*events, terminal], indent=2) + "\n")
            verify(events, terminal)
