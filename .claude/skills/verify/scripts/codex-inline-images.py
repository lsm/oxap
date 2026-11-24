import argparse
import base64
import json
from pathlib import Path
import runpy
import random
import re
import struct
import sys
import tempfile
import zlib

HUB = runpy.run_path(str(Path(__file__).with_name("deepseek-native-resume.py")))
TEXT_BEFORE = {"type": "text", "text": "Name the three vertical stripes from left to right."}
TEXT_AFTER = {"type": "text", "text": "Reply with three color words. Do not use tools."}
COLORS = {
    "red": b"\xff\x00\x00", "blue": b"\x00\x00\xff", "green": b"\x00\xff\x00",
    "yellow": b"\xff\xff\x00", "black": b"\x00\x00\x00", "white": b"\xff\xff\xff",
}


def png(colors):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))

    row = b"\0" + b"".join(COLORS[color] * 24 for color in colors)
    pixels = row * 64
    return base64.b64encode(
        b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 72, 64, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(pixels)) + chunk(b"IEND", b"")
    ).decode()


def image(data, mime="image/png"):
    return {"type": "image", "image": {"media_type": mime, "data": data}}


def native(default_model):
    def emit(value):
        print(json.dumps(value), flush=True)

    turn = 0
    for line in sys.stdin:
        call = json.loads(line)
        method = call["method"]
        params = call.get("params") or {}
        if method == "initialize":
            result = {"userAgent": "fixture", "codexHome": "/fixture", "platformFamily": "unix", "platformOs": "macos"}
        elif method == "model/list":
            if params.get("cursor") == "vision-page":
                result = {"data": [{"id": "vision", "model": "vision", "isDefault": True,
                                    "inputModalities": ["text", "image"]}], "nextCursor": None}
            else:
                result = {"data": [{"id": "text-only", "model": "text-only", "isDefault": False,
                                    "inputModalities": ["text"]}], "nextCursor": "vision-page"}
        elif method == "config/read":
            result = {"config": {"model": default_model}}
        elif method == "thread/start":
            result = {"thread": {"id": "thread"}, "model": params.get("model", default_model or "vision")}
        elif method == "turn/start":
            turn += 1
            with Path("turns.jsonl").open("a") as log:
                log.write(json.dumps(params["input"]) + "\n")
            result = {"turn": {"id": f"turn-{turn}", "status": "inProgress"}}
        else:
            continue
        emit({"id": call["id"], "result": result})
        if method == "turn/start":
            for status, event in [("inProgress", "started"), ("completed", "completed")]:
                emit({"method": f"turn/{event}", "params": {
                    "threadId": "thread", "turn": {"id": f"turn-{turn}", "status": status}}})


def verify(binary, root, model, codex=None, native_model=None, verb="serve", zig=True):
    (root / "home").mkdir(parents=True)
    config = root / "hub.json"
    live = codex is not None
    entry = {"type": "codex", "executable": str(codex or Path(sys.executable).resolve()),
             "working_directory": str(root), "sandbox": "read-only", "approval_policy": "never",
             "environment": ["HOME", "PATH"]}
    if model:
        entry["model"] = model
    if not live:
        entry["args"] = [str(Path(__file__).resolve()), "--native", native_model or ""]
    config.write_text(json.dumps({"adapters": {"codex": entry}}))
    command = [str(binary), verb, "--stdio", "--config", str(config),
               "--session-history", str(root / "sessions.jsonl")]
    if zig:
        command.append("--no-save-oapx-sessions")
    hub = HUB["Hub"](command, root, "hub", home=Path.home() if live else None)
    colors = random.SystemRandom().sample(list(COLORS), 3) if live else ["red", "green", "blue"]
    data = png(colors)
    parts = [TEXT_BEFORE, image(data), TEXT_AFTER]

    def submit(content, override=None, revision=None):
        payload = {"session_id": "s1", "delivery": "auto",
                   "messages": [{"role": "user", "content": content}]}
        if override:
            payload["model_id"] = override
        request = HUB["envelope"]("session.message.submit.request", payload)
        if revision:
            request["capability_revision"] = revision
        return hub.call("submit", wait=90, session_id="s1", request=request)

    def refused(reply, code):
        assert not reply["ok"] and reply["error"]["code"] == code, reply

    text_only = (model or native_model) == "text-only"
    try:
        capabilities = hub.call("capabilities", adapter="codex")["result"]
        features = capabilities["payload"]["features"]
        revision = capabilities["capability_revision"]
        opened = hub.call("open", wait=90, adapter="codex", request=HUB["envelope"](
            "session.open.request", {"session_id": "s1"}))
        assert opened["ok"], opened
        assert hub.call("events", session_id="s1")["ok"]
        if not live and text_only:
            reply = submit(parts, revision=revision)
            refused(reply, "unsupported_feature")
            assert "does not support inline images" in reply["error"]["message"], reply
            assert "content.image" not in features, features
            assert submit([TEXT_BEFORE, TEXT_AFTER], revision=revision)["ok"]
            expected = [TEXT_BEFORE, TEXT_AFTER]
        else:
            if not live:
                reply = submit(parts, revision=revision)
                assert reply["ok"], f"mixed text and PNG refused: {reply}"
                expected = [TEXT_BEFORE, {"type": "image", "url": "data:image/png;base64," + data}, TEXT_AFTER]
                hub.completed("s1")
                refused(submit(parts, "text-only"), "unsupported_feature")
                for bad in (
                    {"type": "image", "image": {"url": "https://example.invalid/a.png"}},
                    image("bad!"), image("ab=="), image(data, "text/plain"),
                    image(base64.b64encode(bytes((4 << 20) + 1)).decode()),
                ):
                    refused(submit([bad]), "invalid_submission")
                refused(submit([image("")]), "schema_invalid")
                large = image(base64.b64encode(bytes(3 << 20)).decode())
                refused(submit([large, large]), "invalid_submission")
                refused(submit([large, {"type": "text", "text": "x" * (4 << 20)}]), "invalid_submission")
                assert submit("still usable")["ok"]
            else:
                assert submit(parts, revision=revision)["ok"]
            assert features["content.image"]["level"] == "native", features
        assert list(features) == sorted(features), features
        hub.completed("s1")
        if live:
            deltas = [frame["envelope"]["payload"]["part"].get("text", "") for frame in hub.seen
                      if frame.get("envelope", {}).get("type") == "content.delta"]
            answer = re.findall(r"[a-z]+", "".join(deltas).lower())
            assert answer == colors, f"expected {colors!r}; received {answer!r}"
            print(f"PASS: Codex returned the synthetic PNG's stripe colors in order: {', '.join(answer)}")
        else:
            observed = [json.loads(line) for line in (root / "turns.jsonl").read_text().splitlines()]
            want = [expected] if text_only else [expected, [{"type": "text", "text": "still usable"}]]
            assert observed == want, f"native inputs differed: calls={len(observed)}"
            starts = [frame["envelope"] for frame in hub.seen
                      if frame.get("envelope", {}).get("type") == "run.started"]
            assert [event["capability_revision"] for event in starts] == [revision] * len(want)
        hub.stop()
        print(f"{model or native_model or 'default'}: hub exit={hub.process.returncode}, "
              f"completed turns={1 if live else len(observed)}")
        return revision
    finally:
        hub.cleanup()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--native"]:
        native(sys.argv[2] or None)
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("binary", type=Path)
        parser.add_argument("--codex", type=Path)
        parser.add_argument("--model")
        parser.add_argument("--hub", choices=("zig", "go", "rust"), default="zig")
        args = parser.parse_args()
        with tempfile.TemporaryDirectory(prefix="oap-codex-images-") as temp:
            root = Path(temp).resolve()
            binary = args.binary.resolve()
            options = {"verb": "hub" if args.hub == "go" else "serve", "zig": args.hub == "zig"}
            if args.codex:
                verify(binary, root, args.model, args.codex.resolve(), **options)
            else:
                vision = verify(binary, root / "vision", "vision", **options)
                assert verify(binary, root / "default", None, **options) == vision
                assert verify(binary, root / "text-only", "text-only", **options) != vision
                assert verify(binary, root / "native-text", None, native_model="text-only", **options) != vision
                print("PASS: 4 Codex model routes")
