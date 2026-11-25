import argparse
import json
from pathlib import Path
import runpy
import secrets
import sys
import tempfile

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
            (store / f"start-{starts}").write_text(matches[0])
            result = {"thread": {"id": f"thread-{starts}"}}
        else:
            continue
        print(json.dumps({"id": frame["id"], "result": result}), flush=True)


def verify(binary, root):
    grants = root / "grants"
    grants.mkdir(mode=0o700)
    (root / "home").mkdir()
    expected = {}
    canaries = []
    descriptor = {"id": "tools", "kind": "remote", "protocol": "mcp"}
    config = root / "hub.json"
    config.write_text(json.dumps({"adapters": {"codex": {
        "type": "codex", "executable": str(Path(sys.executable).resolve()),
        "args": [str(Path(__file__).resolve()), "--fixture", str(root)],
        "working_directory": str(root), "model": "fixture", "environment": [],
        "approval_policy": "never", "sandbox": "read-only"}}}))

    def issue(session, port, source=descriptor):
        reference, bearer = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        canaries.extend((reference, bearer))
        endpoint = f"http://127.0.0.1:{port}/mcp"
        headers = {"Authorization": "Bearer " + bearer}
        path = grants / reference
        path.touch(mode=0o600)
        path.write_text(json.dumps({
            "session_id": session, "source": {**source, "endpoint": endpoint}, "headers": headers}))
        return reference, {"url": endpoint, "http_headers": headers}

    alpha, expected["alpha"] = issue("alpha", 10001)
    beta, expected["beta"] = issue("beta", 10002)
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

    def refused(session, *sources):
        reply = opened(session, *sources)
        error = reply.get("error", {})
        assert error.get("code") == "unsupported_feature", "open did not refuse the attachment"
        assert error.get("details") == {"feature": ATTACH, "reason": "unsatisfiable", "source": "tools"}

    try:
        reply = hub.call("capabilities", adapter="codex")
        feature = reply["result"]["payload"]["features"][ATTACH]
        assert feature["level"] == "native" and set(feature["modes"]) == {"session_open", "remote"}
        assert feature["limits"]["transports"] == ["remote"]
        refused("alpha", attach(None))
        refused("alpha", attach("../not-a-reference"))
        wrong_source, _ = issue("alpha", 10003, {**descriptor, "id": "other"})
        refused("alpha", attach(wrong_source))
        foreign, _ = issue("foreign", 10004)
        refused("alpha", attach(foreign))
        duplicate, _ = issue("alpha", 10005)
        refused("alpha", attach(duplicate), attach(duplicate))
        assert not list(root.glob("start-*")), "a refused open reached thread/start"
        for name, reference in (("alpha", alpha), ("beta", beta)):
            reply = opened(name, attach(reference))
            assert reply.get("ok"), "credential-backed open failed"
            state = reply["result"]["payload"]
            assert [source["id"] for source in state["sources"]] == ["tools"]
            assert not (grants / reference).exists(), "the grant was left reusable"
        assert [path.read_text() for path in sorted(root.glob("start-*"))] == ["alpha", "beta"]
        assert hub.call("close", session_id="alpha")["ok"]
        refused("alpha", attach(alpha))
        hub.stop()
        published = [json.dumps(hub.seen), (root / "hub-stderr.txt").read_text(),
                     (root / "bindings.jsonl").read_text()]
        assert not any(canary in data for data in published for canary in canaries), "published credential material"
        print("hub exit=0, isolated sessions=2, attachment refusals=6, published canary hits=0")
    finally:
        hub.cleanup()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fixture"]:
        fixture(Path(sys.argv[2]))
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("binary", type=Path)
        args = parser.parse_args()
        with tempfile.TemporaryDirectory(prefix="oap-codex-sources-") as directory:
            verify(args.binary.resolve(), Path(directory))
