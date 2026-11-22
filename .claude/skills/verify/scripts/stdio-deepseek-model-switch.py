import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile


EXCHANGE = runpy.run_path(str(Path(__file__).with_name("claude-model-endpoint.py")))["exchange"]
FIXTURE = Path(__file__).resolve().parents[4] / "go/cmd/goap/testdata/parity/deepseek-switch"
PRO = "deepseek-v4-pro"
FLASH = "deepseek-v4-flash"
SWITCH = "session.model.switch"


def verify(binary, root):
    child = root / "child.sh"
    script = (FIXTURE / "child.sh").read_text().replace('"model":"deepseek-chat","provider"', f'"model":"{FLASH}","provider"')
    child.write_text(script.replace("deepseek-chat", PRO))
    child.chmod(0o700)
    config = root / "hub.json"
    registry = json.loads((FIXTURE / "registry.json").read_text().replace("@DIR@", str(root)))
    registry["adapters"]["deepseek"]["model"] = PRO
    config.write_text(json.dumps(registry))
    history = root / "session-history.jsonl"
    frames = [json.loads(line) for line in (FIXTURE / "scenario.jsonl").read_text().splitlines()]
    frames[3]["payload"]["model_id"] = FLASH
    home = root / "home"
    home.mkdir()
    command = [binary, "hub", "--stdio", "--config", str(config), "--session-history", str(history)]
    environment = {"PATH": os.defpath, "HOME": str(home), "NODE_ENV": "test"}
    with subprocess.Popen(command, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0) as hub:
        def call(id, op, **parameters):
            lines = EXCHANGE(hub, [{"id": id, "op": op, **parameters}], lambda line: line.get("id") == id)
            return json.loads(lines.splitlines()[-1])

        def result(reply):
            assert reply["ok"], reply
            return reply["result"]

        try:
            features = result(call(1, "capabilities", adapter="deepseek"))["payload"]["features"]
            assert features[SWITCH]["level"] == "emulated", features[SWITCH]
            result(call(2, "open", adapter="deepseek", request=frames[2]))
            initial = result(call(3, "history"))["sessions"]
            assert len(initial) == 1 and initial[0]["model"] == PRO, initial
            switched = result(call(4, SWITCH, session_id="s1", request=frames[3]))
            assert switched["type"] == SWITCH + ".response" and switched["in_reply_to"] == "m1", switched
            assert switched["payload"] == {"session_id": "s1", "model_id": FLASH, "previous_model_id": PRO}, switched
            state = result(call(5, "state", session_id="s1"))["payload"]
            assert state["current_model_id"] == FLASH, state
            current = result(call(6, "history"))["sessions"]
            assert len(current) == 1 and current[0]["session_id"] == "s1" and current[0]["model"] == FLASH, current
            result(call(7, "events", session_id="s1"))
            events = EXCHANGE(hub, [{"id": 8, "op": "submit", "session_id": "s1", "request": frames[4]}],
                              lambda line: b"run.completed" in json.dumps(line).encode())
            submitted = result(next(reply for reply in map(json.loads, events.splitlines()) if reply.get("id") == 8))
            assert submitted["payload"]["model_id"] == FLASH, submitted
            refused = call(9, SWITCH, session_id="s1", request=frames[6])
            assert not refused["ok"] and refused["error"]["code"] == "unsupported_feature", refused
            assert refused["error"]["details"]["reason"] == "unsatisfiable", refused
            hub.stdin.close()
            assert hub.wait(timeout=10) == 0, f"hub exit={hub.returncode}"
        finally:
            if hub.poll() is None:
                hub.kill()
                hub.wait(timeout=10)

    written = [json.loads(line.split(" ", 1)[1]) for line in history.read_text().splitlines()]
    assert [entry["record"]["model"] for entry in written] == [PRO, FLASH], written
    calls = [json.loads(line) for line in (root / "stdin.log").read_text().splitlines() if line.startswith("{")]
    initialized = [call["params"]["model"] for call in calls if call["method"] == "initialize"]
    assert initialized == [PRO, FLASH], initialized
    print(f"hub exit={hub.returncode}, {len(initialized)} runtimes initialized, 1 turn completed on {FLASH}, {len(written)} history records")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="oxap-stdio-model-switch-") as temp:
        verify(str(Path(sys.argv[1]).resolve()), Path(temp))
