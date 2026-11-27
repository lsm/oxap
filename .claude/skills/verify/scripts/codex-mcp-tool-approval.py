import argparse
from contextlib import ExitStack
from http.server import ThreadingHTTPServer
import io
import json
from pathlib import Path
import runpy
import secrets
import tempfile
import threading
import time

CREATE = runpy.run_path(str(Path(__file__).with_name("codex-session-tool-sources.py")))
HUB = CREATE["HUB"]
REPLY = "Fixture tool call settled"


def named(value, suffix):
    if isinstance(value, dict):
        if str(value.get("name", "")).endswith(suffix):
            return [value]
        return [found for child in value.values() for found in named(child, suffix)]
    if isinstance(value, list):
        return [found for child in value for found in named(child, suffix)]
    return []


class Responses(CREATE["FixtureServer"]):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path != "/model/responses":
            label = self.path.removeprefix("/")
            if body.get("method") == "tools/call" and self.headers.get("Authorization") == self.server.routes.get(label):
                self.server.mcp_methods.add((label, "tools/call"))
                return self.reply(200, {"jsonrpc": "2.0", "id": body["id"],
                                        "result": {"content": [{"type": "text", "text": f"{label} tool ran"}]}})
            data = json.dumps(body).encode()
            self.headers.replace_header("Content-Length", str(len(data)))
            self.rfile = io.BytesIO(data)
            return super().do_POST()
        serialized = json.dumps(body.get("input", []))
        label = next(label for label in self.server.routes if f"fixture for session {label}" in serialized)
        index = len(self.server.model_requests)
        self.server.model_requests.append(label)
        outputs = [item for item in body.get("input", []) if item.get("type") == "function_call_output"]
        if outputs:
            self.server.tool_outputs[label] = outputs
            item = {"type": "message", "id": f"msg_{index}", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": REPLY, "annotations": []}]}
        else:
            tool = named(body.get("tools", []), f"{label}_tool")
            assert len(tool) == 1, f"model request for {label} did not offer its tool"
            item = {"type": "function_call", "id": f"fc_{index}", "call_id": f"call_{index}",
                    "name": tool[0]["name"], "arguments": "{}", "status": "completed"}
            for entry in body.get("tools", []):
                if entry.get("type") == "namespace" and named(entry.get("tools", []), f"{label}_tool"):
                    item["namespace"] = entry.get("name")
        response = {"id": f"resp_{index}", "status": "completed", "output": [item],
                    "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}
        events = [{"type": "response.created", "response": {"id": response["id"]}},
                  {"type": "response.output_item.done", "output_index": 0, "item": item},
                  {"type": "response.completed", "response": response}]
        data = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def wait_for(hub, session, kind):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        for frame in hub.seen:
            event = frame.get("envelope", {})
            if event.get("session_id") == session and event.get("type") == kind:
                return event
        frame = hub.lines.get(timeout=max(0.1, deadline - time.monotonic()))
        assert frame is not None, "hub exited before the event arrived"
        hub.seen.append(frame)
    raise AssertionError(f"{kind} did not arrive for {session}")


def verify(binary, codex, root, server):
    grants = root / "grants"
    grants.mkdir(mode=0o700)
    home = root / "home"
    home.mkdir()
    codex_home = root / "codex-home"
    CREATE["native_config"](codex_home, server.server_port)
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": {
        "type": "codex", "executable": str(codex), "model": "fixture",
        "working_directory": str(root), "approval_policy": "on-request", "sandbox": "read-only",
        "environment": [f"HOME={home}", f"CODEX_HOME={codex_home}", "PATH=/usr/bin:/bin", "NODE_ENV=test"],
    }}}))
    canaries = []

    def grant(session):
        reference, bearer = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        canaries.extend((reference, bearer))
        server.routes[session] = "Bearer " + bearer
        path = grants / reference
        path.touch(mode=0o600)
        path.write_text(json.dumps({"session_id": session,
                                    "source": {"id": "tools", "kind": "remote", "protocol": "mcp",
                                               "endpoint": f"http://127.0.0.1:{server.server_port}/{session}"},
                                    "headers": {"Authorization": "Bearer " + bearer}}))
        return reference

    hub = HUB["Hub"]([str(binary), "serve", "--stdio", f"--config={config}",
                      "--session-tool-sources", str(grants), "--session-history", str(root / "bindings.jsonl"),
                      "--no-save-oapx-sessions"], root, "hub")
    seen = {}
    try:
        revision = hub.call("capabilities", adapter="codex")["result"]["capability_revision"]
        for session, choice, granted in (("allowed", "accept", True), ("remembered", "acceptForSession", True),
                                         ("denied", "decline", False)):
            reply = hub.call("open", adapter="codex", request=HUB["envelope"]("session.open.request", {
                "session_id": session, "tool_sources": [{"id": "tools", "kind": "remote", "credential_ref": grant(session)}]}))
            assert reply.get("ok"), f"open {session} failed: {reply.get('error')}"
            assert hub.call("events", session_id=session)["ok"]
            request = HUB["envelope"]("session.message.submit.request", {
                "session_id": session, "delivery": "auto",
                "messages": [{"role": "user", "content": f"fixture for session {session}"}]})
            assert hub.call("submit", session_id=session, request=request)["ok"]
            asked = wait_for(hub, session, "action.permission.requested")["payload"]
            offered = [entry["id"] for entry in asked["choices"]]
            assert choice in offered, f"{session} was not offered {choice}: {offered}"
            resolution = HUB["envelope"]("action.permission.resolve.request", {
                "session_id": session, "run_id": asked["run_id"], "interaction_id": asked["interaction_id"],
                "requested_by": asked["requested_by"], "responded_by": asked["responded_by"],
                "choice_id": choice, "granted": granted})
            resolution.update(run_id=asked["run_id"], capability_revision=revision)
            answer = hub.call("resolve", session_id=session, request=resolution)
            assert answer.get("ok"), f"resolve {session} failed: {answer.get('error')}"
            CREATE["wait_terminal"](hub, session)
            resolved = wait_for(hub, session, "action.permission.resolved")["payload"]
            output = json.dumps(server.tool_outputs.get(session, []))
            called = (session, "tools/call") in server.mcp_methods
            seen[session] = (offered, resolved["outcome"], called)
            print(f"{session}: offered={offered} choice={choice} outcome={resolved['outcome']} tools/call={called} model saw={output[:200]}")
            assert called == granted, f"{session}: the MCP call reached the server={called}, expected {granted}"
            assert (f"{session} tool ran" in output) == granted
        hub.stop()
        published = [json.dumps(hub.seen), (root / "hub-stderr.txt").read_text(), (root / "bindings.jsonl").read_text()]
        assert not any(canary in data for data in published for canary in canaries), "published credential material"
        print(f"hub exit=0, permission requests resolved=3, calls reached on grant=2, calls reached on decline=0, canary hits=0")
    finally:
        hub.cleanup()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    with ExitStack() as stack:
        directory = stack.enter_context(tempfile.TemporaryDirectory(prefix="oap-codex-mcp-approval-"))
        server = stack.enter_context(ThreadingHTTPServer(("127.0.0.1", 0), Responses))
        server.routes = {}
        server.model_requests = []
        server.tool_outputs = {}
        server.mcp_methods = set()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.shutdown)
        verify(args.binary.resolve(), args.codex.resolve(), Path(directory), server)
