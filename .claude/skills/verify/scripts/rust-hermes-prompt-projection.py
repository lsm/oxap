import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def frames(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def gateway(corpus):
    def read():
        return json.loads(sys.stdin.readline())

    def send(frame):
        print(frame if isinstance(frame, str) else json.dumps(frame), flush=True)

    send(frames(corpus / "ready-handshake/native.jsonl")[0]["raw"])
    assert read() == {"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}}
    send({"jsonrpc": "2.0", "id": 1, "result": {
        "session_id": "sess0001", "stored_session_id": "stored",
    }})
    for frame in frames(corpus / "streaming-provenance/native.jsonl"):
        if frame["direction"] == "host-to-gateway":
            assert read() == frame["raw"]
        elif frame["direction"] == "gateway-to-host":
            send(frame["raw"])
    assert sys.stdin.read() == ""


def verify(binary, corpus):
    with tempfile.TemporaryDirectory(prefix="hermes-projection-") as directory:
        root = Path(directory)
        module = root / "tui_gateway"
        module.mkdir()
        script = Path(__file__).resolve()
        (module / "entry.py").write_text(
            f"import runpy\n"
            f"from pathlib import Path\n"
            f"runpy.run_path({str(script)!r})['gateway'](Path({str(corpus)!r}))\n"
        )
        result = subprocess.run(
            [binary, sys.executable, directory, directory, "--project"],
            env={"PATH": os.defpath, "HOME": directory, "NODE_ENV": "test"},
            timeout=45, capture_output=True, text=True,
        )
    print(result.stdout, end="")
    print(result.stderr, end="", file=sys.stderr)
    print(f"GATEWAY EXAMPLE EXIT: {result.returncode}")
    result.check_returncode()
    lines = result.stdout.splitlines()
    deltas = [json.loads(line[7:]) for line in lines if line.startswith("delta: ")]
    completed = [json.loads(line[11:]) for line in lines if line.startswith("completed: ")]
    shutdowns = lines.count("gateway exited cleanly")
    print(f"DELTAS: {len(deltas)}\nCOMPLETIONS: {len(completed)}\nCLEAN SHUTDOWNS: {shutdowns}")
    expected = json.loads((corpus / "streaming-provenance/expected-oap.json").read_text())
    assert deltas == [entry["payload"]["part"] for entry in expected if entry["type"] == "content.delta"]
    assert completed == [
        {"text": entry["payload"]["final_response"]["content"], "usage": entry["payload"]["usage"]}
        for entry in expected if entry["type"] == "run.completed"
    ]
    assert lines.count("prompt started at native sequence 1") == shutdowns == 1


if __name__ == "__main__":
    verify(str(Path(sys.argv[1]).resolve()), Path("fixtures/adapters/hermes-v0.23.3").resolve())
