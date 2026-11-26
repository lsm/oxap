import argparse
from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import runpy
import secrets
import sys
import tempfile
import threading
import time

HUB = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
ATTACH = "action.tool_sources.attach"


def fixture(store):
    starts = len(list(store.glob("start-*")))
    for line in sys.stdin:
        frame = json.loads(line)
        method = frame["method"]
        if method == "initialize":
            result = {"userAgent": "fixture", "codexHome": "/fixture",
                      "platformFamily": "unix", "platformOs": "linux"}
        elif method == "model/list":
            result = {"data": [], "nextCursor": None}
        elif method == "thread/start":
            params = frame["params"]
            expected = json.loads((store / "expected.json").read_text())
            config = params.get("config", {})
            matches = [label for label, server in expected.items()
                       if config.get("mcp_servers") == {"tools": server}]
            if len(matches) != 1 or config.get("model_reasoning_effort") != "high":
                raise SystemExit("thread/start did not receive the expected per-session MCP configuration")
            if matches == ["refused"]:
                authorization = config["mcp_servers"]["tools"]["http_headers"]["Authorization"]
                print(authorization, file=sys.stderr, flush=True)
                print(json.dumps({"id": frame["id"], "error": {"code": -32602, "message": authorization}}), flush=True)
                continue
            (store / f"start-{starts}").write_text(matches[0])
            result = {"thread": {"id": f"thread-{starts}"}}
        else:
            continue
        print(json.dumps({"id": frame["id"], "result": result}), flush=True)


class FixtureServer(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, body=None):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/model/responses":
            serialized = json.dumps(body.get("input", []))
            labels = [label for label in self.server.routes if f"fixture for session {label}" in serialized]
            inventory = json.dumps(body.get("tools", []))
            seen = {candidate for candidate in self.server.routes if f'{candidate}_tool"' in inventory}
            self.server.model_observations.append((labels, sorted(seen)))
            for label in labels:
                self.server.model_tools[label] = seen
            self.reply(400, {"error": {"message": "Local fixture completed its observation",
                                      "type": "invalid_request_error"}})
            return
        label = self.path.removeprefix("/")
        authorization = self.server.routes.get(label)
        if authorization is None or self.headers.get("Authorization") != authorization:
            self.reply(401)
            return
        self.server.mcp_methods.add((label, body["method"]))
        if "id" not in body:
            self.reply(202)
            return
        method = body["method"]
        if method == "initialize":
            result = {"protocolVersion": body["params"]["protocolVersion"], "capabilities": {"tools": {}},
                      "serverInfo": {"name": label, "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": f"{label}_tool", "description": "Fixture tool",
                                 "inputSchema": {"type": "object"}}]}
        else:
            result = {}
        self.reply(200, {"jsonrpc": "2.0", "id": body["id"], "result": result})


def native_config(codex_home, port):
    codex_home.mkdir()
    (codex_home / "config.toml").write_text(f"""
model_provider = "fixture"
web_search = "disabled"

[features]
plugins = false

[model_providers.fixture]
name = "Fixture"
base_url = "http://127.0.0.1:{port}/model"
wire_api = "responses"
""")


def wait_terminal(hub, session):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        for frame in hub.seen:
            event = frame.get("envelope", {})
            if event.get("session_id") == session and event.get("type") in {
                    "run.completed", "run.failed", "run.cancelled"}:
                return
        frame = hub.lines.get(timeout=max(0.1, deadline - time.monotonic()))
        assert frame is not None, "hub exited before the turn settled"
        hub.seen.append(frame)
    raise AssertionError("local fixture turn did not settle")


def verify(binary, root, codex, server):
    grants = root / "grants"
    grants.mkdir(mode=0o700)
    (root / "home").mkdir()
    expected = {}
    canaries = []
    descriptor = {"id": "tools", "kind": "remote", "protocol": "mcp"}
    codex_home = root / "codex-home"
    adapter = {"type": "codex", "executable": str(Path(sys.executable).resolve()),
               "args": [str(Path(__file__).resolve()), "--fixture", str(root)],
               "working_directory": str(root), "model": "fixture", "environment": [],
               "approval_policy": "never", "sandbox": "read-only"}
    if codex:
        native_config(codex_home, server.server_port)
        adapter.update(executable=str(codex), args=[], environment=[
            f"HOME={root / 'home'}", f"CODEX_HOME={codex_home}", "PATH=/usr/bin:/bin", "NODE_ENV=test"])
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": adapter}}))

    def issue(session, port, source=descriptor):
        reference, bearer = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        canaries.extend((reference, bearer))
        endpoint = f"http://127.0.0.1:{port}/mcp"
        headers = {"Authorization": "Bearer " + bearer}
        if server and session in {"alpha", "beta"} and session not in server.routes:
            endpoint = f"http://127.0.0.1:{server.server_port}/{session}"
            server.routes[session] = headers["Authorization"]
        path = grants / reference
        path.touch(mode=0o600)
        path.write_text(json.dumps({
            "session_id": session, "source": {**source, "endpoint": endpoint}, "headers": headers}))
        return reference, {"url": endpoint, "http_headers": headers}

    alpha, expected["alpha"] = issue("alpha", 10001)
    beta, expected["beta"] = issue("beta", 10002)
    rejected, expected["refused"] = issue("refused", 10006)
    if not codex:
        (root / "expected.json").write_text(json.dumps(expected))
    hub = HUB["Hub"]([str(binary), "serve", "--stdio", f"--config={config}",
                      "--session-tool-sources", str(grants), "--session-history", str(root / "bindings.jsonl"),
                      "--no-save-oapx-sessions"], root, "hub")

    def attach(ref, **values):
        item = {"id": "tools", "kind": "remote", **values}
        if ref is not None:
            item["credential_ref"] = ref
        return item

    def opened(session, *sources):
        return hub.call("open", adapter="codex", request=HUB["envelope"]("session.open.request", {
            "session_id": session, "tool_sources": list(sources), "reasoning_level": "high"}))

    def refused(session, *sources, source="tools"):
        reply = opened(session, *sources)
        error = reply.get("error", {})
        assert error.get("code") == "unsupported_feature", "open did not refuse the attachment"
        assert error.get("details") == {"feature": ATTACH, "reason": "unsatisfiable", "source": source}

    try:
        reply = hub.call("capabilities", adapter="codex")
        feature = reply["result"]["payload"]["features"][ATTACH]
        assert feature["level"] == "native" and set(feature["modes"]) == {"session_open", "remote"}
        assert feature["limits"]["transports"] == ["remote"]
        refused("alpha", attach(None))
        refused("alpha", attach("../not-a-reference"))
        refused("alpha", attach(secrets.token_urlsafe(32)))
        invalid_mode, _ = issue("alpha", 10007)
        (grants / invalid_mode).chmod(0o644)
        refused("alpha", attach(invalid_mode))
        wrong_source, _ = issue("alpha", 10003, {**descriptor, "id": "other"})
        refused("alpha", attach(wrong_source))
        foreign, _ = issue("foreign", 10004)
        refused("alpha", attach(foreign))
        duplicate, _ = issue("alpha", 10005)
        refused("alpha", attach(duplicate), attach(duplicate))
        assert (grants / duplicate).is_file(), "a duplicate attachment consumed a valid grant"
        refused("alpha", attach(alpha), attach(None, id="missing"), source="missing")
        assert (grants / alpha).is_file(), "an unresolved sibling consumed a valid grant"
        literal = opened("alpha", attach(None, headers={"Authorization": "Bearer " + canaries[-1]}))
        assert literal.get("error", {}).get("code") == "schema_invalid", "wire accepted literal headers"
        assert not list(root.glob("start-*")), "a refused open reached thread/start"
        for name, reference in (("alpha", alpha), ("beta", beta)):
            reply = opened(name, attach(reference))
            assert reply.get("ok"), "credential-backed open failed"
            state = reply["result"]["payload"]
            assert [source["id"] for source in state["sources"]] == ["tools"]
            assert not (grants / reference).exists(), "the grant was left reusable"
        if codex:
            for session in ("alpha", "beta"):
                assert hub.call("events", session_id=session)["ok"]
                request = HUB["envelope"]("session.message.submit.request", {
                    "session_id": session, "delivery": "auto",
                    "messages": [{"role": "user", "content": f"fixture for session {session}"}]})
                assert hub.call("submit", session_id=session, request=request)["ok"]
                wait_terminal(hub, session)
            print(f"native model observations={server.model_observations}, MCP calls={sorted(server.mcp_methods)}")
            assert server.model_tools == {"alpha": {"alpha"}, "beta": {"beta"}}, "native model tool catalogs crossed or were empty"
        else:
            assert [path.read_text() for path in sorted(root.glob("start-*"))] == ["alpha", "beta"]
            reply = opened("refused", attach(rejected))
            assert reply.get("error", {}).get("code") == "probe_failed", "native open failure was lost"
            assert not (grants / rejected).exists(), "a failed native open left its grant reusable"
            refused("refused", attach(rejected))
        assert hub.call("close", session_id="alpha")["ok"]
        refused("alpha", attach(alpha))
        hub.stop()
        published = [json.dumps(hub.seen), (root / "hub-stderr.txt").read_text(),
                     (root / "bindings.jsonl").read_text()]
        assert not any(canary in data for data in published for canary in canaries), "published credential material"
        if codex:
            files = [path for path in codex_home.rglob("*") if path.is_file()]
            rollouts = sum(path.name.startswith("rollout-") for path in files)
            assert rollouts == 2, f"expected two native rollouts, saw {rollouts}"
            needles = [canary.encode() for canary in canaries]
            for path in files:
                content = path.read_bytes()
                assert not any(needle in content for needle in needles), "credential material in Codex home"
            print(f"native model requests with isolated tools=2, scanned files={len(files)}, rollouts={rollouts}, file canary hits=0")
        print(f"hub exit=0, isolated sessions=2, attachment refusals={9 if codex else 10}, published canary hits=0")
    finally:
        hub.cleanup()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fixture"]:
        fixture(Path(sys.argv[2]))
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("binary", type=Path)
        parser.add_argument("--codex", type=Path)
        args = parser.parse_args()
        with ExitStack() as stack:
            directory = stack.enter_context(tempfile.TemporaryDirectory(prefix="oap-codex-sources-"))
            server = None
            if args.codex:
                server = stack.enter_context(ThreadingHTTPServer(("127.0.0.1", 0), FixtureServer))
                server.routes = {}
                server.model_tools = {}
                server.model_observations = []
                server.mcp_methods = set()
                threading.Thread(target=server.serve_forever, daemon=True).start()
                stack.callback(server.shutdown)
            verify(args.binary.resolve(), Path(directory), args.codex.resolve() if args.codex else None, server)
