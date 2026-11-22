import argparse
from contextlib import contextmanager, nullcontext, suppress
from dataclasses import dataclass
import http.server
import json
import os
from pathlib import Path
import re
import select
import shlex
import subprocess
import sys
import tempfile
import threading
import time

from harness_delivery_peers import PACKED, REPLY, SESSION, THOUGHT, TOOL_ARGS, TOOL_RESULT


ROOT = Path(__file__).resolve().parents[4]
PROFILE = "open-agent-protocol.agent-control-core"
OPENCODE_SESSION = "ses_fake00000000000000"


@dataclass(frozen=True)
class Case:
    tool: str = "read"
    arguments: dict | None = None
    stop: str = "end_turn"
    registries: tuple[str, ...] = ("zig", "go")
    reasoning_gap: str = ""
    result: object = None


CASES = {
    "acp": Case(tool="Read file", registries=("zig", "go", "rust"), reasoning_gap="agent_thought_chunk"),
    "claude": Case(tool="Read", stop="completed", result=TOOL_RESULT["text"]),
    "codex": Case(tool="codex.command", arguments={"command": "printf fixture", "cwd": "/"},
                  result={"command": "printf fixture", "cwd": "/", "exitCode": 0,
                          "output": TOOL_RESULT["text"]}, registries=("zig", "go", "rust"),
                  reasoning_gap="item/reasoning/summaryTextDelta"),
    "copilot": Case(registries=("go", "rust")),
    "deepseek": Case(stop="completed", result=[{"type": "text", **TOOL_RESULT}],
                     registries=("zig", "go", "rust")),
    "hermes": Case(stop="completed"),
    "memory": Case(tool="scripted_tool", arguments={"operation": "golden"}, result={"ok": True},
                   registries=("zig", "go", "rust")),
    "oapx": Case(tool="", registries=("zig",)),
    "opencode": Case(stop="stop", result=[{"type": "text", **TOOL_RESULT}]),
    "pi": Case(stop="stop"),
}


def check_registries():
    zig = (ROOT / "zig/src/tools/makai.zig").read_text().split("const HubRegistry = struct", 1)[1]
    zig = zig.split("fn reported(", 1)[0]
    go = (ROOT / "go/serve/registry.go").read_text().split("func buildAdapter(", 1)[1]
    go = go.split("func claudeToolPosture(", 1)[0]
    rust = (ROOT / "rust/serve/src/config.rs").read_text().split("match kind.as_str() {", 1)[1]
    rust = rust.split("registry.register(&name, built)?;", 1)[0]
    for name, kinds in {
        "zig": re.findall(r'entry\.kind, "([a-z][a-z0-9_-]*)"\)', zig),
        "go": re.findall(r'^\s*case "([a-z][a-z0-9_-]*)":', go, re.M),
        "rust": re.findall(r'^\s*"([a-z][a-z0-9_-]*)"\s*=>', rust, re.M),
    }.items():
        covered = {kind for kind, case in CASES.items() if name in case.registries}
        assert set(kinds) == covered, f"{name} registry: missing cases={set(kinds) - covered}, stale cases={covered - set(kinds)}"


class Fixture(http.server.BaseHTTPRequestHandler):
    def send_json(self, obj):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(obj).encode())

    def do_GET(self):
        if self.path == "/api/event":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            self.event({"id": "evt_connected", "type": "server.connected", "data": {}})
            self.server.prompt.wait(15)
            if not self.server.events:
                return
            with suppress(BrokenPipeError, ConnectionResetError):
                for item in self.server.events:
                    self.event(item)
                self.server.finished.wait(15)
        elif self.path == "/v1/models":
            self.send_json({"data": [{"id": "fixture"}]})
        elif self.path == "/api/session/active":
            self.send_json({"data": {}})
        else:
            self.send_error(404)

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/api/session":
            self.send_json({"data": {"id": OPENCODE_SESSION, "projectID": "prj_fake",
                            "model": {"id": "fixture", "providerID": "fixture"},
                            "time": {"created": 1, "updated": 1}}})
        elif self.path == f"/api/session/{OPENCODE_SESSION}/prompt":
            self.send_json({"data": {"id": request["id"], "sessionID": OPENCODE_SESSION, "time": {"created": 1},
                            "type": "user", "payload": {"text": request["text"]},
                            "delivery": request["delivery"]}})
            for seq, (kind, data) in enumerate((
                ("inbox.delivered", {"inboxID": request["id"]}),
                ("step.started", {"agent": "build", "model": {"id": "fixture", "providerID": "fixture"},
                                  "started": 1}),
                ("reasoning.delta", {"ordinal": 0, "delta": THOUGHT[:6]}),
                ("reasoning.ended", {"ordinal": 0, "text": THOUGHT}),
                ("text.delta", {"ordinal": 1, "delta": REPLY[:5]}),
                ("text.ended", {"ordinal": 1, "text": REPLY}),
                ("tool.input.started", {"id": "call", "name": "read"}),
                ("tool.called", {"id": "call", "input": TOOL_ARGS, "executed": True}),
                ("tool.success", {"id": "call", "content": [{"type": "text", **TOOL_RESULT}], "executed": True}),
                ("step.ended", {"finish": "stop", "cost": 0,
                                "tokens": {"input": 1, "output": 1, "reasoning": 0,
                                           "cache": {"read": 0, "write": 0}}}),
                ("execution.succeeded", {}),
            ), 1):
                if kind not in ("inbox.delivered", "execution.succeeded"):
                    data["assistantMessageID"] = "msg_a1"
                event = {"id": f"evt_{seq}", "created": seq, "type": f"session.{kind}",
                         "data": {"sessionID": OPENCODE_SESSION, **data}}
                if kind not in ("reasoning.delta", "text.delta"):
                    event["durable"] = {"aggregateID": OPENCODE_SESSION, "seq": seq, "version": 1}
                self.server.events.append(event)
            self.server.prompt.set()
        elif self.path == "/v1/chat/completions":
            assert "Authorization" not in self.headers
            assert request["messages"][-1]["content"] == "Verify delivery."
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.end_headers()
            for delta, finish in (({"role": "assistant", "reasoning_content": THOUGHT}, None),
                                  ({"content": REPLY[:5]}, None), ({"content": REPLY[5:]}, None),
                                  ({}, "stop")):
                self.event({"id": "chatcmpl-fixture", "object": "chat.completion.chunk", "model": "fixture",
                            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
            self.wfile.write(b"data: [DONE]\n\n")
        else:
            self.send_error(404)

    def event(self, event):
        self.wfile.write(f"data: {json.dumps(event)}\n\n".encode())
        self.wfile.flush()

    def log_message(self, *_):
        pass


@contextmanager
def fake_http():
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as server:
        server.prompt = threading.Event()
        server.finished = threading.Event()
        server.events = []
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}"
        finally:
            server.finished.set()
            server.prompt.set()
            server.shutdown()
            thread.join()


def configuration(root, kind, url):
    entry = {"type": kind, "working_directory": str(root)}
    if kind == "opencode":
        entry["endpoint"] = url
    elif kind == "oapx":
        state = root / ".oxap"
        state.mkdir()
        (state / "providers.json").write_text(json.dumps({"providers": [{
            "id": "journey", "api": "openai-completions", "base_url": url,
            "auth": "none", "reasoning": True, "models": ["fixture"],
        }]}))
    elif kind != "memory":
        peer = Path(__file__).with_name("harness_delivery_peers.py")
        launcher = root / "peer"
        invocation = shlex.join([sys.executable, str(peer), kind])
        launcher.write_text(f'#!/bin/sh\nexec {invocation} 2>>{shlex.quote(str(root / "peer.stderr"))}\n')
        launcher.chmod(0o700)
        entry["executable"] = str(launcher)
        entry["environment"] = ["HOME", "NODE_ENV", "OXAP_STATE_DIR", "TMPDIR"]
    entry.update({
        "claude": {"unrestricted_tools": True},
        "codex": {"model": "fixture", "approval_policy": "on-request", "sandbox": "workspace-write"},
        "deepseek": {"provider": "deepseek-official", "model": "deepseek-v4-flash"},
        "hermes": {"model": "fixture"},
    }.get(kind, {}))
    return {"adapters": {kind: entry}}


def envelope(kind, run=None, **payload):
    scope = {"session_id": SESSION}
    if run:
        scope["run_id"] = run
    return {"type": kind, "id": kind, "protocol": "open-agent-protocol", "version": "0.1",
            "profile": PROFILE, **scope, "payload": {**scope, **payload}}


def exchange(hub, root, kind, slow, evidence):
    def send(id_, op, **fields):
        hub.stdin.write((json.dumps({"id": id_, "op": op, **fields}) + "\n").encode())

    deadline = time.monotonic() + 15
    buffered = b""

    def read():
        nonlocal buffered
        while b"\n" not in buffered:
            assert select.select([hub.stdout], [], [], max(0, deadline - time.monotonic()))[0], "hub timed out"
            chunk = os.read(hub.stdout.fileno(), 65536)
            assert chunk, f"hub closed output (exit={hub.poll()})"
            buffered += chunk
        line, buffered = buffered.split(b"\n", 1)
        evidence.write(line + b"\n")
        evidence.flush()
        return json.loads(line)

    def ack(id_):
        frame = read()
        assert frame["id"] == id_ and frame.get("ok"), frame
        return frame["result"]

    send(1, "open", adapter=kind, request=envelope("session.open.request"))
    assert ack(1)["type"] == "session.open.response"
    send(2, "events", session_id=SESSION)
    ack(2)
    controls = {
        "oapx": {"model_id": "journey/openai-chat-completions@fixture"},
        "memory": {"output_schema": {"type": "object", "required": ["ok"]}},
    }.get(kind, {})
    send(3, "submit", session_id=SESSION, request=envelope("session.message.submit.request",
         delivery="auto", messages=[{"role": "user", "content": "Verify delivery."}], **controls))

    if slow:
        while not (root / "native-turn-ended").exists():
            assert hub.poll() is None and time.monotonic() < deadline, "native turn did not end"
            time.sleep(0.01)

    run_id = None
    events = []
    pending = {3}
    while pending or not events or events[-1]["type"] != "run.completed":
        frame = read()
        if frame["id"] in pending:
            pending.remove(frame["id"])
            assert frame.get("ok") and frame["result"]["payload"]["accepted"], frame
            if frame["id"] == 3:
                run_id = frame["result"]["payload"]["run_id"]
            continue
        assert frame["id"] == 2 and frame.get("event") == "envelope", frame
        event = frame["envelope"]
        assert event["session_id"] == SESSION, event
        assert event["sequence"] == frame["sequence"] == len(events) + 1, event
        events.append(event)
        assert event["type"] not in ("run.failed", "run.cancelled"), event
        payload = event["payload"]
        if kind == "memory" and event["type"] in ("action.permission.requested", "user.input.requested"):
            resolve = dict(interaction_id=payload["interaction_id"], requested_by=payload["requested_by"],
                           responded_by=payload["responded_by"])
            if event["type"] == "action.permission.requested":
                id_, method, answer = 4, "action.permission", {"granted": True, "choice_id": "approve"}
            else:
                id_, method = 5, "user.input"
                answer = {"answers": [{"question_id": "choice", "selected_option_ids": ["yes"]}]}
            send(id_, "resolve", session_id=SESSION,
                 request=envelope(method + ".resolve.request", event["run_id"], **resolve, **answer))
            pending.add(id_)

    assert run_id and all(event["run_id"] == run_id for event in events)
    hub.stdin.close()
    hub.stdin = None
    tail, _ = hub.communicate(timeout=5)
    tail = buffered + tail
    evidence.write(tail)
    assert hub.returncode == 0, f"hub exit={hub.returncode}"
    for line in tail.splitlines():
        frame = json.loads(line)
        assert frame.get("event") == "oap-session-closed", frame
    return events


def check(kind, case, events, strict):
    streamed = []
    deltas = []
    calls = []
    for event in events:
        payload = event["payload"]
        if event["type"] == "content.delta":
            deltas.append(event["sequence"])
            part = payload["part"]
            part_kind = part["type"]
            text = part.get("text", part.get("reasoning", part.get("thinking")))
            assert text, part
            part_kind = "reasoning" if part_kind == "thinking" else part_kind
            if streamed and streamed[-1][0] == part_kind:
                streamed[-1] = (part_kind, streamed[-1][1] + text)
            else:
                streamed.append((part_kind, text))
        if event["type"].startswith("action.call."):
            calls.append(event)
    assert events[0]["type"] == "run.started", events[0]
    terminal = events[-1]["payload"]
    assert terminal["stop_reason"] == case.stop, terminal
    final = terminal["final_response"]
    assert final["role"] == "assistant", final
    content = final["content"]
    final_text = content if isinstance(content, str) else "".join(
        part["text"] for part in content if part["type"] == "text")
    expected_text = "The golden script completed." if kind == "memory" else REPLY
    assert final_text == expected_text, final
    expected_stream = [("reasoning", "".join(PACKED) if kind == "deepseek" else THOUGHT), ("text", REPLY)]
    if kind == "memory":
        assert terminal["result"] == {"ok": True}, terminal
        expected_stream = [("text", "I will use the scripted tool." + expected_text)]
    if case.tool:
        assert [event["type"] for event in calls] == [
            "action.call.requested", "action.call.started", "action.call.completed"], calls
        asked, started, completed = (event["payload"] for event in calls)
        assert all(tool["name"] == case.tool and tool["tool_call_id"] == asked["tool_call_id"]
                   for tool in (asked, started, completed)), calls
        assert asked["arguments_json"] == (case.arguments or TOOL_ARGS), asked
        if kind == "memory":
            assert deltas[0] < calls[0]["sequence"] < calls[-1]["sequence"] < deltas[-1], calls
        else:
            assert deltas and deltas[-1] < calls[0]["sequence"], calls
        assert completed["result"] == (case.result if case.result is not None else TOOL_RESULT), completed
    else:
        assert not calls, calls
    gap = case.reasoning_gap and not strict
    if gap:
        expected_stream = expected_stream[1:]
    assert streamed == expected_stream, (
        f"{kind} stream received {[(part, len(value)) for part, value in streamed]}; "
        f"expected {[(part, len(value)) for part, value in expected_stream]}"
    )
    return len(events), len(calls)


def verify(kind, case, binary, slow, output, runtime, strict):
    if kind == "oapx" and sys.platform == "darwin":
        raise AssertionError("the built-in adapter's model catalog can import from the macOS Keychain; run this case on Linux")
    label = f"{runtime}-{kind}" + ("-slow" if slow else "")
    endpoint = fake_http() if kind in ("opencode", "oapx") else nullcontext(None)
    with tempfile.TemporaryDirectory(prefix="harness-delivery-") as work, endpoint as url:
        root = Path(work).resolve()
        config = root / "registry.json"
        config.write_text(json.dumps(configuration(root, kind, url)))
        env = {"HOME": str(root), "TMPDIR": str(root), "PATH": os.defpath, "NODE_ENV": "test",
               "OXAP_STATE_DIR": str(root / ".oxap"),
               "OXAP_KEYCHAIN_SERVICE": root.name}
        command = [str(binary), "serve", "--stdio", f"--config={config}"]
        if runtime == "zig":
            command.append("--no-save-oapx-sessions")
        with (output / f"{label}.jsonl").open("wb") as evidence, (output / f"{label}.stderr").open("wb") as stderr:
            with subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=stderr, bufsize=0) as hub:
                try:
                    events = exchange(hub, root, kind, slow, evidence)
                finally:
                    if hub.poll() is None:
                        hub.kill()
                        hub.wait(timeout=5)
                    peer_stderr = root / "peer.stderr"
                    if peer_stderr.exists():
                        stderr.write(peer_stderr.read_bytes())
        event_count, call_count = check(kind, case, events, strict)
        gap = f", known missing reasoning={case.reasoning_gap}" if case.reasoning_gap else ""
        print(f"{label}: hub exit=0, events={event_count}, tool events={call_count}, completed=1{gap}", flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--oxap", required=True, type=Path)
    parser.add_argument("--goap", required=True, type=Path)
    parser.add_argument("--rust-hub", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--case", choices=CASES, action="append")
    parser.add_argument("--runtime", choices=("zig", "go", "rust"), action="append")
    parser.add_argument("--strict-reasoning", action="store_true")
    args = parser.parse_args()
    check_registries()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    binaries = {"zig": args.oxap.resolve(), "go": args.goap.resolve(), "rust": args.rust_hub.resolve()}
    failures = 0
    count = 0
    for kind in args.case or CASES:
        case = CASES[kind]
        for runtime in case.registries:
            if args.runtime and runtime not in args.runtime:
                continue
            for slow in (False, True) if kind == "deepseek" else (False,):
                count += 1
                try:
                    verify(kind, case, binaries[runtime], slow, args.output_dir, runtime, args.strict_reasoning)
                except (AssertionError, KeyError, OSError, subprocess.TimeoutExpired) as error:
                    failures += 1
                    print(f"{runtime}-{kind} (slow={slow}): {error}", file=sys.stderr, flush=True)
    if not count:
        parser.error("no registered cases match the selected runtimes")
    print(f"journeys={count}, failures={failures}")
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
