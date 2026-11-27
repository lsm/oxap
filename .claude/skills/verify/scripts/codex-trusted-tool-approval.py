import argparse
import io
from contextlib import ExitStack
from http.server import ThreadingHTTPServer
import json
from pathlib import Path
import runpy
import secrets
import tempfile
import threading

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


def namespaced(tools, suffix):
    for tool in tools:
        if tool.get("type") == "namespace" and named(tool.get("tools", []), suffix):
            return tool.get("name")
    return None


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
        self.server.model_requests.append((label, body))
        outputs = [item for item in body.get("input", []) if item.get("type") in {"function_call_output", "custom_tool_call_output"}]
        if outputs:
            self.server.tool_outputs[label] = outputs
            item = {"type": "message", "id": f"msg_{index}", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": REPLY, "annotations": []}]}
        else:
            tool = named(body.get("tools", []), f"{label}_tool")
            assert len(tool) == 1, f"model request for {label} did not offer its tool"
            item = {"type": "function_call", "id": f"fc_{index}", "call_id": f"call_{index}",
                    "name": tool[0]["name"], "arguments": "{}", "status": "completed"}
            namespace = namespaced(body.get("tools", []), f"{label}_tool")
            if namespace:
                item["namespace"] = namespace
        response = {"id": f"resp_{index}", "status": "completed", "output": [item],
                    "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}
        events = [
            {"type": "response.created", "response": {"id": response["id"]}},
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": response},
        ]
        data = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def verify(binary, codex, root, server, policy):
    grants = root / "grants"
    grants.mkdir(mode=0o700)
    home = root / "home"
    home.mkdir()
    codex_home = root / "codex-home"
    CREATE["native_config"](codex_home, server.server_port)
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": {
        "type": "codex", "executable": str(codex), "model": "fixture",
        "working_directory": str(root), "approval_policy": policy, "sandbox": "read-only",
        "environment": [f"HOME={home}", f"CODEX_HOME={codex_home}", "PATH=/usr/bin:/bin", "NODE_ENV=test"],
    }}}))
    canaries = []

    def grant(session, approval):
        reference, bearer = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        canaries.extend((reference, bearer))
        server.routes[session] = "Bearer " + bearer
        document = {"session_id": session,
                    "source": {"id": "tools", "kind": "remote", "protocol": "mcp",
                               "endpoint": f"http://127.0.0.1:{server.server_port}/{session}"},
                    "headers": {"Authorization": "Bearer " + bearer}}
        if approval:
            document["tool_approval"] = approval
        path = grants / reference
        path.touch(mode=0o600)
        path.write_text(json.dumps(document))
        return reference

    hub = HUB["Hub"]([str(binary), "serve", "--stdio", f"--config={config}",
                      "--session-tool-sources", str(grants), "--session-history", str(root / "bindings.jsonl"),
                      "--no-save-oapx-sessions"], root, "hub")
    try:
        invalid = grant("invalid", "prompt")
        reply = hub.call("open", adapter="codex", request=HUB["envelope"]("session.open.request", {
            "session_id": "invalid", "tool_sources": [{"id": "tools", "kind": "remote", "credential_ref": invalid}]}))
        assert reply.get("error", {}).get("code") == "unsupported_feature", "an unknown tool_approval was accepted"
        for session, approval in (("approved", "approve"), ("plain", None)):
            reference = grant(session, approval)
            reply = hub.call("open", adapter="codex", request=HUB["envelope"]("session.open.request", {
                "session_id": session, "tool_sources": [{"id": "tools", "kind": "remote", "credential_ref": reference}]}))
            assert reply.get("ok"), f"open {session} failed: {reply.get('error')}"
            assert "tool_approval" not in json.dumps(reply["result"]), "the approval setting was published"
            assert hub.call("events", session_id=session)["ok"]
            request = HUB["envelope"]("session.message.submit.request", {
                "session_id": session, "delivery": "auto",
                "messages": [{"role": "user", "content": f"fixture for session {session}"}]})
            assert hub.call("submit", session_id=session, request=request)["ok"]
            CREATE["wait_terminal"](hub, session)
        calls = {label: sorted(method for seen, method in server.mcp_methods if seen == label and method == "tools/call")
                 for label in ("approved", "plain")}
        outputs = {label: [item.get("output") for item in items] for label, items in server.tool_outputs.items()}
        print(f"approval_policy={policy} MCP tools/call by session={calls}")
        print(f"tool outputs the model saw={json.dumps(outputs)[:600]}")
        permission_requests = [frame for frame in hub.seen if frame.get("envelope", {}).get("type") == "action.permission.requested"]
        print(f"permission requests published={len(permission_requests)}")
        assert calls == {"approved": ["tools/call"], "plain": []}, "approval did not gate exactly the unapproved source"
        assert "approved tool ran" in json.dumps(outputs["approved"]), "the approved call's result did not reach the model"
        hub.stop()
        published = [json.dumps(hub.seen), (root / "hub-stderr.txt").read_text(), (root / "bindings.jsonl").read_text()]
        assert not any(canary in data for data in published for canary in canaries), "published credential material"
        files = [path for path in codex_home.rglob("*") if path.is_file()]
        needles = [canary.encode() for canary in canaries]
        hits = sum(any(needle in path.read_bytes() for needle in needles) for path in files)
        assert hits == 0, "credential material in Codex home"
        print(f"hub exit=0, approved tool calls=1, unapproved tool calls=0, invalid approval refusals=1, scanned files={len(files)}, canary hits=0")
    finally:
        hub.cleanup()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=Path)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--approval-policy", default="on-request")
    args = parser.parse_args()
    with ExitStack() as stack:
        directory = stack.enter_context(tempfile.TemporaryDirectory(prefix="oap-codex-approval-"))
        server = stack.enter_context(ThreadingHTTPServer(("127.0.0.1", 0), Responses))
        server.routes = {}
        server.model_requests = []
        server.tool_outputs = {}
        server.mcp_methods = set()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.shutdown)
        verify(args.binary.resolve(), args.codex.resolve(), Path(directory), server, args.approval_policy)
