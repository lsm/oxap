import argparse
from contextlib import ExitStack
import fcntl
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
PRO = "deepseek-v4-pro"
FLASH = "deepseek-v4-flash"
SWITCH = "session.model.switch"
INTERRUPTED = "run three tools"


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
    leases = {}

    def acquire(session):
        lease = (store / (session + ".lock")).open("a")
        try:
            fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            lease.close()
            return False
        leases[session] = lease
        return True

    for line in sys.stdin:
        request = json.loads(line)
        method, params, number = request.get("method"), request.get("params") or {}, request.get("id")
        log({"method": method, "params": params})
        if method == "initialize":
            model = params["model"]
            emit({"id": number, "jsonrpc": "2.0", "result": {"serverInfo": SERVER}})
        elif method == "session/resume":
            if (store / "stall-resume").exists():
                continue
            session = params["sessionId"]
            history = saved(session)
            if history is None or session in live:
                emit({"id": number, "jsonrpc": "2.0", "error": {"code": -32000, "message": "no stored session " + session}})
            elif not acquire(session):
                emit({"id": number, "jsonrpc": "2.0", "error": {"code": -32000, "message": "native session lease is held"}})
            else:
                live[session] = history
                emit({"id": number, "jsonrpc": "2.0", "result": {}})
        elif method == "session/prompt":
            session = params["sessionId"]
            if session not in live:
                assert saved(session) is None and acquire(session), "prompt must follow resume for a stored session"
            history = live.setdefault(session, [])
            text = "".join(block["text"] for block in params["contentBlocks"])
            history.append(text)
            (store / (session + ".json")).write_text(json.dumps(history))
            turn = len(history)
            reply = "history=" + json.dumps(history)
            user = {"content": [{"text": text, "type": "text"}], "id": f"m-{turn}", "role": "user", "source": {"kind": "user"}}
            assistant = {"content": [{"text": reply, "type": "text"}], "id": f"a-{turn}", "role": "assistant",
                         "source": {"kind": "model", "model": model, "provider": "fixture-provider"}}

            def event(seq, kind, data, **extra):
                emit({"jsonrpc": "2.0", "method": "session.event", "params": {
                    "event": {"data": data, "seq": seq, "time": seq, "type": kind, **extra}, "sessionId": session}})

            def status(value):
                emit({"jsonrpc": "2.0", "method": "session.status", "params": {"sessionId": session, "status": value}})

            base = turn * 20
            event(base + 1, "agent/inbox/spliced", {"inserted": [user], "start": 0, "target": "next-turn"})
            emit({"id": number, "jsonrpc": "2.0", "result": {"messageId": user["id"]}})
            status("running")
            event(base + 2, "turn/start", {"turn": turn})
            event(base + 3, "step/start", {"step": 1, "turn": turn})
            event(base + 4, "user/message", user, surfaceOp="append")
            if text == INTERRUPTED:
                for index in range(3):
                    call_id = f"tool-{index}"
                    seq = base + 5 + index * 2
                    event(seq, "tool/call", {"turn": turn, "step": 1, "callId": call_id, "name": "read", "arguments": "{}"})
                    if index < 2:
                        result = {"id": f"result-{index}", "role": "tool", "toolCallId": call_id,
                                  "source": {"kind": "tool", "callId": call_id},
                                  "content": [{"type": "text", "text": "fixture result"}]}
                        event(seq + 1, "tool/result", {"turn": turn, "step": 1, "message": result}, surfaceOp="append")
                continue
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
                    return event
            line = self.lines.get(timeout=30)
            assert line is not None, "hub exited before the turn completed"
            self.seen.append(line)
        raise AssertionError("turn did not complete")

    def stop(self):
        self.process.stdin.close()
        code = self.process.wait(timeout=15)
        self.errors.close()
        assert code == 0, f"hub exit={code}"

    def cleanup(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=15)
        self.process.stdin.close()
        self.process.stdout.close()
        self.errors.close()


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


def submit(hub, session, text, model):
    request = envelope("session.message.submit.request", {"session_id": session, "delivery": "auto",
                                                         "messages": [{"role": "user", "content": text}]})
    reply = hub.call("submit", session_id=session, request=request)
    assert reply["ok"] and reply["result"]["payload"]["model_id"] == model, reply


def turn(hub, session, text, model, revision):
    submit(hub, session, text, model)
    completed = hub.completed(session)
    assert completed["capability_revision"] == revision, (completed["capability_revision"], revision)
    content = completed["payload"]["final_response"]["content"]
    return "".join(part.get("text", "") for part in content) if isinstance(content, list) else content


def switch(hub, session, revision):
    request = envelope(SWITCH + ".request", {"session_id": session, "model_id": FLASH})
    request["capability_revision"] = revision
    return hub.call(SWITCH, session_id=session, request=request)


def wait_for_lease_release(store, native):
    with (store / (native + ".lock")).open("a") as lease:
        deadline = time.monotonic() + 10
        while True:
            try:
                fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
                return
            except BlockingIOError:
                assert time.monotonic() < deadline, "native process kept the lease after the hub died"
                time.sleep(0.02)


def verify(name, command, root, cleanup):
    root.mkdir()
    (root / "home").mkdir()
    store = root / "store"
    store.mkdir()
    launcher = root / "dsh"
    launcher.write_text(f"#!/bin/sh\nexec {sys.executable} -I {Path(__file__).resolve()} --runtime {store}\n")
    launcher.chmod(0o700)
    (root / "hub.json").write_text(json.dumps({"adapters": {"deepseek": {
        "type": "deepseek", "executable": str(launcher), "working_directory": str(root),
        "provider": "fixture-provider", "model": PRO, "environment": ["PATH"]}}}))
    command = [*command, "--config", str(root / "hub.json"), "--session-history", str(root / "history.jsonl")]

    first = Hub(command, root, "first")
    cleanup.callback(first.cleanup)
    capabilities = first.call("capabilities", adapter="deepseek")
    level = reopen_level(capabilities)
    revision = capabilities["result"]["capability_revision"]
    assert level == "native", f"{name}: session.open.reopen is {level}"
    opened(first, "s1")
    native = link(first, "s1")
    assert native != "s1", f"{name}: a fresh open reused the logical id as the native id"
    switched = switch(first, "s1", revision)
    assert switched["ok"] and switched["result"]["payload"]["previous_model_id"] == PRO, switched
    assert first.call("events", session_id="s1")["ok"]
    reply = turn(first, "s1", "remember ZEBRA", FLASH, revision)
    assert reply == 'history=["remember ZEBRA"]', reply
    submit(first, "s1", INTERRUPTED, FLASH)
    deadline = time.monotonic() + 10
    events = [line["envelope"]["type"] for line in first.seen if "envelope" in line]
    while events.count("action.call.started") < 3:
        try:
            line = first.lines.get(timeout=max(0.1, deadline - time.monotonic()))
        except queue.Empty as err:
            raise AssertionError(f"tool calls were not delivered: {events}") from err
        assert line is not None and time.monotonic() < deadline, "tool calls were not delivered"
        if "envelope" in line:
            events.append(line["envelope"]["type"])
    assert events.count("action.call.completed") == 2, events
    first.process.kill()
    assert first.process.wait(timeout=15) == -9
    wait_for_lease_release(store, native)

    second = Hub(command, root, "second")
    cleanup.callback(second.cleanup)
    state = opened(second, "s1", reopen=True)
    assert link(second, "s1") == native, f"{name}: the reopened session changed its native id"
    switched = switch(second, "s1", revision)
    calls = map(json.loads, (store / "calls.jsonl").read_text().splitlines())
    initialized = [call["params"]["model"] for call in calls if call["method"] == "initialize"]
    assert switched["ok"] and switched["result"]["payload"]["previous_model_id"] == FLASH, (initialized, switched)
    assert state["current_model_id"] == FLASH and initialized == [PRO, FLASH, FLASH], (state, initialized)
    assert second.call("events", session_id="s1")["ok"]
    reply = turn(second, "s1", "which word?", FLASH, revision)
    assert reply == 'history=["remember ZEBRA", "run three tools", "which word?"]', f"{name}: the reopened turn lost the conversation: {reply}"
    assert opened(second, "s2")["current_model_id"] == PRO
    fresh = link(second, "s2")
    assert fresh not in ("s2", native), f"{name}: a fresh open did not get its own native id"
    second.stop()

    (store / "stall-resume").touch()
    third = Hub(command, root, "third")
    cleanup.callback(third.cleanup)
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
    assert prompts == [native, native, native], f"{name}: prompts went to {prompts}"
    resumed_at = next(i for i, call in enumerate(calls) if call["method"] == "session/resume")
    assert calls[resumed_at - 1]["method"] == "initialize", f"{name}: session/resume did not follow initialize"
    print(f"{name}: killed during the third tool after two results, reopened on {FLASH} with one resume on the same native id, next turn retained history, hub exit=0, a silent resume was refused after {waited:.0f}s")


def main():
    if sys.argv[1:2] == ["--runtime"]:
        fake_runtime(Path(sys.argv[2]))
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, help="Zig oxap binary")
    parser.add_argument("--goap", type=Path, help="Go goap binary")
    parser.add_argument("--rust-hub", type=Path, help="Rust oap-rust-hub binary")
    args = parser.parse_args()
    hubs = []
    if args.binary:
        hubs.append(("zig", [str(args.binary.resolve()), "serve", "--stdio"]))
    if args.goap:
        hubs.append(("go", [str(args.goap.resolve()), "hub", "--stdio"]))
    if args.rust_hub:
        hubs.append(("rust", [str(args.rust_hub.resolve()), "serve", "--stdio"]))
    assert hubs, "name at least one hub binary"
    with tempfile.TemporaryDirectory(prefix="oxap-deepseek-resume-") as directory:
        for name, command in hubs:
            with ExitStack() as cleanup:
                verify(name, command, Path(directory).resolve() / name, cleanup)
    print(f"PASS: {len(hubs)} hub(s) resumed a DeepSeek conversation across a process restart")


if __name__ == "__main__":
    main()
