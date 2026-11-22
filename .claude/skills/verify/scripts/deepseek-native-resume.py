import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
import uuid


SERVER = {"name": "deepseek-harness-sdk-runtime", "version": "0.0.1"}


def fake_runtime(store):
    def emit(message):
        sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
        sys.stdout.flush()

    def log(entry):
        with open(store / "calls.jsonl", "a") as calls:
            calls.write(json.dumps(entry) + "\n")

    def saved(session):
        path = store / (session + ".json")
        return json.loads(path.read_text()) if path.exists() else None

    live = {}
    for line in sys.stdin:
        request = json.loads(line)
        method, params, number = request.get("method"), request.get("params") or {}, request.get("id")
        log({"method": method, "params": params})
        if method == "initialize":
            emit({"id": number, "jsonrpc": "2.0", "result": {"serverInfo": SERVER}})
        elif method == "session/resume":
            if (store / "stall-resume").exists():
                continue
            session = params["sessionId"]
            history = saved(session)
            if history is None or session in live:
                emit({"id": number, "jsonrpc": "2.0", "error": {"code": -32000, "message": "no stored session " + session}})
            else:
                live[session] = history
                emit({"id": number, "jsonrpc": "2.0", "result": {}})
        elif method == "session/prompt":
            session = params["sessionId"]
            history = live.setdefault(session, [])
            text = "".join(block["text"] for block in params["contentBlocks"])
            history.append(text)
            (store / (session + ".json")).write_text(json.dumps(history))
            turn = len(history)
            reply = "history=" + json.dumps(history)
            user = {"content": [{"text": text, "type": "text"}], "id": f"m-{turn}", "role": "user", "source": {"kind": "user"}}
            assistant = {"content": [{"text": reply, "type": "text"}], "id": f"a-{turn}", "role": "assistant",
                         "source": {"kind": "model", "model": "deepseek-chat", "provider": "fixture-provider"}}

            def event(seq, kind, data, **extra):
                emit({"jsonrpc": "2.0", "method": "session.event", "params": {
                    "event": {"data": data, "seq": seq, "time": seq, "type": kind, **extra}, "sessionId": session}})

            def status(value):
                emit({"jsonrpc": "2.0", "method": "session.status", "params": {"sessionId": session, "status": value}})

            base = turn * 10
            event(base + 1, "agent/inbox/spliced", {"inserted": [user], "start": 0, "target": "next-turn"})
            emit({"id": number, "jsonrpc": "2.0", "result": {"messageId": user["id"]}})
            status("running")
            event(base + 2, "turn/start", {"turn": turn})
            event(base + 3, "step/start", {"step": 1, "turn": turn})
            event(base + 4, "user/message", user, surfaceOp="append")
            event(base + 6, "assistant/message", {"message": assistant, "step": 1, "turn": turn,
                                                  "stream": [{"type": "chunk", "time": base + 6, "chunk": {"type": "text-delta", "index": 0, "text": reply}}],
                                                  "usage": {"inputTokens": 1, "outputTokens": 1}}, surfaceOp="append")
            event(base + 7, "step/end", {"step": 1, "turn": turn})
            event(base + 8, "turn/end", {"reason": {"kind": "completed"}, "turn": turn})
            status("idle")
        elif method == "shutdown":
            emit({"id": number, "jsonrpc": "2.0", "result": {}})
            return


def envelope(kind, payload):
    return {"protocol": "open-agent-protocol", "version": "0.1",
            "profile": "open-agent-protocol.agent-control-core", "type": kind,
            "id": str(uuid.uuid4()), "session_id": payload["session_id"], "payload": payload}


class Hub:
    def __init__(self, command, root, label):
        self.errors = (root / (label + "-stderr.txt")).open("w")
        env = {"PATH": os.environ["PATH"], "HOME": str(root / "home"), "NODE_ENV": "test"}
        self.process = subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=self.errors, text=True)
        self.lines = queue.Queue()
        self.seen = []
        threading.Thread(target=self.read, daemon=True).start()
        self.number = 0

    def read(self):
        for line in self.process.stdout:
            self.lines.put(json.loads(line))
        self.lines.put(None)

    def call(self, op, wait=30, **fields):
        self.number += 1
        self.process.stdin.write(json.dumps({"id": self.number, "op": op, **fields}) + "\n")
        self.process.stdin.flush()
        while True:
            line = self.lines.get(timeout=wait)
            assert line is not None, "hub exited; inspect its stderr"
            self.seen.append(line)
            if line.get("id") == self.number:
                return line

    def completed(self, session):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            for line in self.seen:
                event = line.get("envelope") or {}
                if event.get("type") == "run.completed" and event.get("payload", {}).get("session_id") == session:
                    self.seen.remove(line)
                    return event["payload"]["final_response"]["content"]
            line = self.lines.get(timeout=30)
            assert line is not None, "hub exited before the turn completed"
            self.seen.append(line)
        raise AssertionError("turn did not complete")

    def stop(self):
        self.process.stdin.close()
        code = self.process.wait(timeout=15)
        self.errors.close()
        assert code == 0, f"hub exit={code}"


def opened(hub, session, reopen=False):
    reply = hub.call("open", adapter="deepseek", request=envelope("session.open.request", {"session_id": session, "reopen": reopen}))
    assert "error" not in reply and reply["result"]["type"] == "session.open.response", f"open {session} reopen={reopen}: {reply}"
    return reply["result"]["payload"]


def reopen_level(value):
    if isinstance(value, dict):
        feature = value.get("session.open.reopen")
        if isinstance(feature, dict):
            return feature.get("level")
        value = list(value.values())
    if isinstance(value, list):
        for item in value:
            found = reopen_level(item)
            if found:
                return found
    return None


def link(hub, session):
    reply = hub.call("work.status", session_id=session)
    assert "error" not in reply, reply
    found = reply["result"]["link"]
    assert found.startswith("deepseek://sessions/"), found
    return found.removeprefix("deepseek://sessions/")


def turn(hub, session, text):
    events = hub.call("events", session_id=session)
    assert "error" not in events, events
    request = envelope("session.message.submit.request", {"session_id": session, "delivery": "auto",
                                                         "messages": [{"role": "user", "content": text}]})
    reply = hub.call("submit", session_id=session, request=request)
    assert "error" not in reply, reply
    content = hub.completed(session)
    return "".join(part.get("text", "") for part in content) if isinstance(content, list) else content


def verify(name, command, root):
    root.mkdir()
    (root / "home").mkdir()
    store = root / "store"
    store.mkdir()
    launcher = root / "dsh"
    launcher.write_text(f"#!/bin/sh\nexec {sys.executable} -I {Path(__file__).resolve()} --runtime {store}\n")
    launcher.chmod(0o700)
    (root / "hub.json").write_text(json.dumps({"adapters": {"deepseek": {
        "type": "deepseek", "executable": str(launcher), "working_directory": str(root),
        "provider": "fixture-provider", "model": "deepseek-chat", "environment": ["PATH"]}}}))
    command = [*command, "--config", str(root / "hub.json"), "--session-history", str(root / "history.jsonl")]

    first = Hub(command, root, "first")
    level = reopen_level(first.call("capabilities", adapter="deepseek"))
    assert level == "native", f"{name}: session.open.reopen is {level}"
    opened(first, "s1")
    native = link(first, "s1")
    assert native != "s1", f"{name}: a fresh open reused the logical id as the native id"
    reply = turn(first, "s1", "remember ZEBRA")
    assert reply == 'history=["remember ZEBRA"]', reply
    first.stop()

    second = Hub(command, root, "second")
    opened(second, "s1", reopen=True)
    assert link(second, "s1") == native, f"{name}: the reopened session changed its native id"
    reply = turn(second, "s1", "which word?")
    assert reply == 'history=["remember ZEBRA", "which word?"]', f"{name}: the reopened turn lost the conversation: {reply}"
    opened(second, "s2")
    fresh = link(second, "s2")
    assert fresh not in ("s2", native), f"{name}: a fresh open did not get its own native id"
    second.stop()

    (store / "stall-resume").touch()
    third = Hub(command, root, "third")
    started = time.monotonic()
    stalled = third.call("open", wait=120, adapter="deepseek", request=envelope("session.open.request", {"session_id": "s1", "reopen": True}))
    waited = time.monotonic() - started
    assert "error" in stalled or stalled.get("result", {}).get("type") != "session.open.response", f"{name}: a reopen dsh never answered still opened: {stalled}"
    assert "unsatisfiable" in json.dumps(stalled), f"{name}: a reopen dsh never answered was not refused as unsatisfiable: {stalled}"
    assert third.call("capabilities", adapter="deepseek").get("ok"), f"{name}: the hub stopped serving after a stalled resume"
    third.stop()
    (store / "stall-resume").unlink()

    calls = [json.loads(line) for line in (store / "calls.jsonl").read_text().splitlines()]
    resumes = [call["params"]["sessionId"] for call in calls if call["method"] == "session/resume"]
    assert resumes == [native, native], f"{name}: session/resume calls {resumes}, want the stored id twice"
    prompts = [call["params"]["sessionId"] for call in calls if call["method"] == "session/prompt"]
    assert prompts == [native, native], f"{name}: prompts went to {prompts}"
    resumed_at = next(i for i, call in enumerate(calls) if call["method"] == "session/resume")
    assert calls[resumed_at - 1]["method"] == "initialize", f"{name}: session/resume did not follow initialize"
    print(f"{name}: fresh native id, hub exit=0, reopened session resumed {native[:8]}… after initialize, second turn saw the first, a silent resume was refused after {waited:.0f}s")


def main():
    if sys.argv[1:2] == ["--runtime"]:
        fake_runtime(Path(sys.argv[2]))
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, help="Zig oxap binary")
    parser.add_argument("--goap", type=Path, help="Go goap binary")
    args = parser.parse_args()
    hubs = []
    if args.binary:
        hubs.append(("zig", [str(args.binary.resolve()), "serve", "--stdio"]))
    if args.goap:
        hubs.append(("go", [str(args.goap.resolve()), "hub", "--stdio"]))
    assert hubs, "name --binary, --goap, or both"
    with tempfile.TemporaryDirectory(prefix="oxap-deepseek-resume-") as directory:
        for name, command in hubs:
            verify(name, command, Path(directory).resolve() / name)
    print(f"PASS: {len(hubs)} hub(s) resumed a DeepSeek conversation across a process restart")


if __name__ == "__main__":
    main()
