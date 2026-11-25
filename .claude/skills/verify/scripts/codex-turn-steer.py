import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import runpy
import shlex
import threading

HUB = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
GUIDANCE = "Change the reply to STEERED."


class Responses(BaseHTTPRequestHandler):
    def do_POST(self):
        assert self.path == "/v1/responses", self.path
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.requests.append(body)
        index = len(self.server.requests)
        if index == 1:
            self.server.sampling.set()
            assert self.server.release.wait(45), "steer did not arrive during sampling"
        inputs = json.dumps(body["input"])
        text = "STEERED" if GUIDANCE in inputs else "ORIGINAL"
        message = {"type": "message", "id": f"msg_{index}", "role": "assistant", "status": "completed",
                   "content": [{"type": "output_text", "text": text, "annotations": []}]}
        response = {"id": f"resp_{index}", "status": "completed", "output": [message],
                    "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}
        events = [
            {"type": "response.created", "response": {"id": response["id"]}},
            {"type": "response.output_item.added", "output_index": 0, "item": {**message, "content": []}},
            {"type": "response.output_text.delta", "output_index": 0, "item_id": message["id"],
             "content_index": 0, "delta": text},
            {"type": "response.output_item.done", "output_index": 0, "item": message},
            {"type": "response.completed", "response": response},
        ]
        encoded = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, *_):
        pass


def verify(binary, codex, root, server):
    home = root / "home"
    (home / ".codex").mkdir(parents=True)
    (home / ".codex/config.toml").write_text(f"""
model = "gpt-5.4"
model_provider = "local_fixture"
[model_providers.local_fixture]
name = "Local fixture"
base_url = "http://127.0.0.1:{server.server_port}/v1"
wire_api = "responses"
""")
    wire = root / "native-requests.jsonl"
    native_events = root / "native-output.jsonl"
    launcher = root / "codex"
    launcher.write_text(f"""#!/bin/sh
tee -a {shlex.quote(str(wire))} | {shlex.quote(str(codex))} "$@" | tee -a {shlex.quote(str(native_events))}
""")
    launcher.chmod(0o700)
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": {
        "type": "codex", "executable": str(launcher), "working_directory": str(root),
        "sandbox": "read-only", "approval_policy": "never", "environment": ["HOME", "PATH"],
    }}}))
    hub = HUB["Hub"]([str(binary), "serve", "--stdio", "--config", str(config),
                      "--session-history", str(root / "sessions.jsonl"), "--no-save-oapx-sessions"],
                     root, "hub")

    def submit(delivery, content, **fields):
        request = HUB["envelope"]("session.message.submit.request", {
            "session_id": "s1", "delivery": delivery, "messages": [{"role": "user", "content": content}], **fields})
        return request["id"], hub.call("submit", session_id="s1", request=request)

    try:
        capabilities = hub.call("capabilities", adapter="codex")["result"]["payload"]["features"]
        assert capabilities["session.message.delivery.steer"]["level"] == "native", capabilities
        opened = hub.call("open", adapter="codex", request=HUB["envelope"](
            "session.open.request", {"session_id": "s1"}))
        assert opened["ok"], opened
        assert hub.call("events", session_id="s1")["ok"]
        _, started = submit("auto", "Reply with ORIGINAL.")
        assert started["ok"], started
        run = started["result"]["payload"]["run_id"]
        assert server.sampling.wait(30), "Codex did not start sampling"
        _, wrong = submit("steer", GUIDANCE, target_run_id="stale")
        assert not wrong["ok"] and wrong["error"]["code"] == "invalid_steer_target", wrong
        request_id, steered = submit("steer", GUIDANCE, target_run_id=run)
        assert steered["ok"], steered
        admission = steered["result"]["payload"]
        assert admission["admission"] == "steered" and admission["run_id"] == run, admission
        state = hub.call("state", session_id="s1")["result"]["payload"]
        [active] = state["active_runs"]
        [pending] = active["pending_steers"]
        assert active.get("admitted_submit_requests") == [request_id], f"missing steer admission anchor: {state}"
        assert pending["request_id"] == request_id and pending["submission_id"] == admission["submission_id"], state
        (root / "pending-state.json").write_text(json.dumps(state, indent=2) + "\n")
        server.release.set()
        terminal = hub.completed("s1")
        events = [frame["envelope"] for frame in hub.seen if "envelope" in frame] + [terminal]
        assert [event["run_id"] for event in events if event["type"] == "run.started"] == [run], events
        [applied] = [event for event in events if event["type"] == "run.steer.applied"]
        assert applied["payload"]["request_id"] == request_id, applied
        assert applied["payload"]["submission_id"] == admission["submission_id"], applied
        assert applied["run_id"] == terminal["run_id"] == run, events
        assert admission["target_sequence"] < applied["sequence"] < terminal["sequence"], events
        assert GUIDANCE in json.dumps(server.requests[-1]["input"]), server.requests
        assert "STEERED" in json.dumps(terminal["payload"]["final_response"]), terminal
        _, late = submit("steer", GUIDANCE, target_run_id=run)
        assert not late["ok"] and late["error"]["code"] == "invalid_steer_target", late
        hub.stop()
        calls = [json.loads(line) for line in wire.read_text().splitlines()]
        methods = [call for call in calls if call["method"] in ("turn/start", "turn/steer")]
        assert [call["method"] for call in methods] == ["turn/start", "turn/steer"], methods
        [native_start] = [frame["params"] for frame in map(json.loads, native_events.read_text().splitlines())
                          if frame.get("method") == "turn/started"]
        assert methods[0]["params"]["threadId"] == methods[1]["params"]["threadId"] == native_start["threadId"], methods
        assert methods[1]["params"]["expectedTurnId"] == native_start["turn"]["id"], methods
        (root / "events.json").write_text(json.dumps(events, indent=2) + "\n")
        print(f"PASS: hub exit={hub.process.returncode}, turn/start=1, turn/steer=1, "
              f"pending anchors=1, applied=1, completed=1, sampling requests={len(server.requests)}, stale refusals=2")
    finally:
        server.release.set()
        hub.cleanup()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("codex", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir()
    with ThreadingHTTPServer(("127.0.0.1", 0), Responses) as server:
        server.requests = []
        server.sampling = threading.Event()
        server.release = threading.Event()
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            verify(args.binary.resolve(), args.codex.resolve(), root, server)
        finally:
            server.shutdown()
            thread.join()
