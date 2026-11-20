import argparse
import json
import os
from pathlib import Path
from queue import Queue
import subprocess
import threading

ROOT = Path(__file__).resolve().parents[4]
FIXTURE = ROOT / "go/cmd/goap/testdata/parity/deepseek"


def runtime(out):
    child = out / "child.sh"
    source = (FIXTURE / "child.sh").read_text()
    source = source.replace(
        "take; printf ",
        """if [ "$1" = --patch ]; then cat "$2" >> "$(dirname "$0")/patches"; fi
take
case "$line" in
  *'"model":"missing"'*) printf '%s\\n' '{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"no such model"}}'; exit 0 ;;
esac
printf """,
        1,
    )
    source = source.replace(
        "\ntake\nprintf",
        """
take
case "$line" in
  *'"method":"shutdown"'*) printf '%s\\n' '{"jsonrpc":"2.0","id":2,"result":{}}'; exit 0 ;;
esac
printf""",
        1,
    )
    child.write_text(source.replace('"model":"deepseek-chat"', '"model":"other"'))
    child.chmod(0o700)
    registry = out / "registry.json"
    registry.write_text((FIXTURE / "registry.json").read_text().replace("@DIR@", str(out)))
    return registry


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--hub", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    out = args.output_dir.resolve()
    out.mkdir()
    home = out / "home"
    home.mkdir()
    registry = runtime(out)
    initialize, capabilities, opening, _, submit, state = map(
        json.loads, (FIXTURE / "scenario.jsonl").read_text().splitlines()
    )
    opening["payload"].update(reasoning_level="high", compaction_policy={"kind": "off"})
    submit["id"] = "submit"
    submit["payload"]["allow_degraded_features"] = [
        "session.message.submit", "session.message.delivery.auto"
    ]

    def switch(request_id, model):
        return {
            **state,
            "type": "session.model.switch.request",
            "id": request_id,
            "payload": {"session_id": "s1", "model_id": model},
        }

    events = Queue()
    frames = []
    with (out / "hub.stderr").open("w") as stderr, subprocess.Popen(
        [str(args.hub.resolve()), "serve", "agent", "--backend=deepseek", f"--config={registry}"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=stderr,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "NODE_ENV": "test",
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(home / "config"),
            "XDG_DATA_HOME": str(home / "data"),
        },
    ) as hub:
        def read():
            for line in hub.stdout:
                events.put(line)
            events.put(None)

        reader = threading.Thread(target=read)
        reader.start()

        def receive(predicate):
            for _ in range(30):
                line = events.get(timeout=10)
                assert line is not None, "hub output ended"
                frame = json.loads(line)
                frames.append(frame)
                if predicate(frame):
                    return frame
            raise AssertionError("expected frame did not arrive")

        def request(frame, kind):
            hub.stdin.write(json.dumps(frame) + "\n")
            hub.stdin.flush()
            reply = receive(lambda received: received.get("in_reply_to") == frame["id"])
            assert reply["type"] == kind, reply
            return reply["payload"]

        try:
            request(initialize, "protocol.initialize.response")
            descriptor = request(capabilities, "capabilities.response")
            request(opening, "session.open.response")
            missing = request(switch("missing", "missing"), "error.response")["error"]
            assert missing["code"] == "model_not_found", missing
            assert missing["details"]["model_id"] == "missing", missing
            switched = request(switch("switch", "other"), "session.model.switch.response")
            assert switched == {
                "session_id": "s1", "model_id": "other", "previous_model_id": "deepseek-chat"
            }, switched
            snapshot = next(
                (frame for frame in frames if frame["type"] == "session.state.updated"), None
            )
            if snapshot is None:
                snapshot = receive(lambda frame: frame["type"] == "session.state.updated")
            assert snapshot["payload"]["current_model_id"] == "other", snapshot
            assert descriptor["features"]["session.model.switch"]["level"] == "emulated"
            admitted = request(submit, "session.message.submit.response")
            assert admitted["model_id"] == "other", admitted
            if not any(frame["type"] == "run.completed" for frame in frames):
                receive(lambda frame: frame["type"] == "run.completed")
            refused = request(switch("too-late", "deepseek-chat"), "error.response")["error"]
            assert refused["code"] == "unsupported_feature", refused
            assert refused["details"]["reason"] == "unsatisfiable", refused
            hub.stdin.close()
            assert hub.wait(timeout=10) == 0
        finally:
            if hub.poll() is None:
                hub.kill()
                hub.wait(timeout=10)
            reader.join(timeout=10)
            (out / "oap.jsonl").write_text("".join(json.dumps(frame) + "\n" for frame in frames))

    native = [
        json.loads(line)
        for line in (out / "stdin.log").read_text().splitlines()
        if line.startswith("{")
    ]
    starts = [frame["params"] for frame in native if frame["method"] == "initialize"]
    assert [start["model"] for start in starts] == ["deepseek-chat", "missing", "other"], starts
    assert all(start["reasoningEffort"] == "high" for start in starts), starts
    assert (out / "patches").read_text() == "- id: compaction-basic\n  disabled: true\n" * 3
    assert [frame["method"] for frame in native].count("shutdown") == 2
    print("PASS: three initializes, two shutdowns, one completed run and two structured refusals")


if __name__ == "__main__":
    main()
