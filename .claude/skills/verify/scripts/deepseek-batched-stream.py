import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import time


SESSION = "deepseek-batch"
REASONING = ["Think ", "again. "] * 312
REPLY = ["Final ", "reply."]


def peer():
    def emit(**frame):
        print(json.dumps({"jsonrpc": "2.0", **frame}), flush=True)

    def notify(method, **params):
        emit(method=method, params={"sessionId": SESSION, **params})

    def event(seq, kind, data, **fields):
        notify("session.event", event={"seq": seq, "time": seq, "type": kind,
                                       "data": data, **fields})

    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        if method == "initialize":
            emit(id=request["id"], result={
                "serverInfo": {"name": "deepseek-harness-sdk-runtime", "version": "0.0.1"},
            })
        elif method == "session/prompt":
            user = {"id": "user-1", "role": "user", "source": {"kind": "user"},
                    "content": request["params"]["contentBlocks"]}
            event(1, "agent/inbox/spliced", {"inserted": [user], "start": 0, "target": "next-turn"})
            emit(id=request["id"], result={"messageId": user["id"]})
            notify("session.status", status="running")
            event(2, "turn/start", {"turn": 1})
            event(3, "step/start", {"step": 1, "turn": 1})
            event(4, "user/message", user, surfaceOp="append")
            event(5, "assistant/message", {
                "message": {
                    "id": "assistant-1", "role": "assistant",
                    "source": {"kind": "model", "model": "deepseek-v4-flash",
                               "provider": "deepseek-official"},
                    "content": [{"type": kind, "text": "".join(texts)}
                                for kind, texts in (("reasoning", REASONING), ("text", REPLY))],
                },
                "stream": [{"type": f"{kind}-chunks", "time0": 5, "index": index,
                            "dt": [0] * (len(texts) - 1), "texts": texts}
                           for index, (kind, texts) in enumerate(
                               (("reasoning", REASONING), ("text", REPLY)))],
                "step": 1, "turn": 1,
            }, surfaceOp="append")
            event(6, "step/end", {"step": 1, "turn": 1})
            event(7, "turn/end", {"reason": {"kind": "completed"}, "turn": 1})
            notify("session.status", status="idle")
        elif method == "shutdown":
            emit(id=request["id"], result={})
            return
        else:
            raise ValueError(method)


def proof(binary, wire):
    with tempfile.TemporaryDirectory(prefix="deepseek-batched-stream-") as directory:
        root = Path(directory).resolve()
        config = root / "registry.json"
        config.write_text(json.dumps({"adapters": {"deepseek": {
            "type": "deepseek", "executable": sys.executable,
            "args": [str(Path(__file__).resolve()), "--peer"], "any_directory": True,
            "provider": "deepseek-official", "model": "deepseek-v4-flash",
        }}}))
        with wire.open("x") as evidence, subprocess.Popen(
            [str(binary), "serve", "--stdio", "--no-save-oapx-sessions", "--config", str(config)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={"HOME": str(root), "PATH": os.defpath, "NODE_ENV": "test"}, bufsize=0,
        ) as hub:
            def send(**frame):
                hub.stdin.write((json.dumps(frame) + "\n").encode())

            def envelope(kind, **payload):
                return {"type": kind, "id": kind, "session_id": SESSION,
                        "protocol": "open-agent-protocol", "version": "0.1",
                        "profile": "open-agent-protocol.agent-control-core",
                        "payload": {"session_id": SESSION, **payload}}

            def read():
                assert select.select([hub.stdout], [], [], max(0, deadline - time.monotonic()))[0], "hub timed out"
                line = hub.stdout.readline()
                assert line, "hub closed its output"
                evidence.write(line.decode())
                return json.loads(line)

            deadline = time.monotonic() + 10
            try:
                send(id=1, op="open", adapter="deepseek", directory=str(root),
                     request=envelope("session.open.request"))
                assert (opened := read()).get("ok"), opened
                send(id=2, op="events", session_id=SESSION)
                assert (subscribed := read()).get("ok"), subscribed
                send(id=3, op="submit", session_id=SESSION,
                     request=envelope("session.message.submit.request", delivery="auto",
                                      messages=[{"role": "user", "content": "proof"}]))
                parts = {"reasoning": [], "text": []}
                accepted = completed = False
                while not (accepted and completed):
                    frame = read()
                    if frame["id"] == 3:
                        assert frame.get("ok") and frame["result"]["payload"]["accepted"], frame
                        accepted = True
                        continue
                    assert frame.get("event") == "envelope", frame
                    event = frame["envelope"]
                    if event["type"] == "content.delta":
                        part = event["payload"]["part"]
                        parts[part["type"]].append(part[part["type"]])
                    elif event["type"] == "run.completed":
                        completed = True
                    else:
                        assert event["type"] == "run.started", event
                assert "".join(parts["reasoning"]) == "".join(REASONING)
                assert "".join(parts["text"]) == "".join(REPLY)
                hub.stdin.close()
                assert hub.wait(timeout=5) == 0, hub.stderr.read().decode()
                print(f"hub exit=0, reasoning deltas={len(parts['reasoning'])}, "
                      f"text deltas={len(parts['text'])}, completed=1")
            finally:
                if hub.poll() is None:
                    hub.kill()
                    hub.wait(timeout=5)


if __name__ == "__main__":
    if sys.argv[1:] == ["--peer"]:
        peer()
    else:
        parser = argparse.ArgumentParser()
        parser.add_argument("--binary", type=Path, required=True)
        parser.add_argument("--wire", type=Path, required=True)
        args = parser.parse_args()
        proof(args.binary.resolve(), args.wire)
