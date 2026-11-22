import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def frames(path):
    return [json.loads(line) for line in path.read_text().splitlines()]


def gate_frames(corpus, case):
    opening = []
    for frame in frames(corpus / "streaming-provenance/native.jsonl"):
        opening.append(frame)
        if frame["action"] == "wait-submit":
            break
    withdraw = case == "withdrawn-gate"
    fixture = "interaction-expire" if withdraw else "interaction-gates"
    next_seq = 2
    for frame in frames(corpus / fixture / "native.jsonl"):
        if frame["direction"] != "gateway-to-host":
            continue
        raw = frame["raw"]
        if isinstance(raw, str):
            raw = json.loads(raw)
        params = raw.get("params", {})
        if withdraw and params.get("type") == "request.cancel":
            params["seq"] = next_seq
            next_seq += 1
        elif raw.get("method") not in {"approval", "clarify", "sudo", "secret"}:
            continue
        params["session_id"] = "sess0001"
        opening.append({**frame, "raw": raw})
    opening.append({"direction": "gateway-to-host", "raw": {
        "jsonrpc": "2.0", "method": "event", "params": {
            "type": "message.complete", "session_id": "sess0001", "seq": next_seq,
            "payload": {"status": "complete", "text": "done"},
        },
    }})
    return opening


def answered_frames(corpus):
    for frame in frames(corpus / "interaction-gates/native.jsonl"):
        if frame["direction"] == "harness-control":
            continue
        raw = json.loads(frame["raw"])
        params = raw.get("params", {})
        if params.get("session_id"):
            params["session_id"] = "sess0001"
        yield frame if frame["action"] == "expect-write" else {**frame, "raw": raw}


REOPEN_CASES = {"session-reopen", "session-reopen-auto-continue"}
REOPEN_TIMEOUT_CASES = {"reopen-ready-timeout", "reopen-resume-timeout"}


def gateway(corpus, case):
    def read():
        return json.loads(sys.stdin.readline())

    def send(frame):
        print(frame if isinstance(frame, str) else json.dumps(frame), flush=True)

    if case == "reopen-ready-timeout":
        assert sys.stdin.read() == ""
        return
    if case == "reopen-resume-timeout":
        ready, request, _ = frames(corpus / "session-reopen/native.jsonl")
        send(ready["raw"])
        assert sys.stdin.readline() == request["raw"] + "\n"
        Path("resume-received").touch()
        assert sys.stdin.read() == ""
        return
    if case != "malformed-frame" and case not in REOPEN_CASES:
        send(frames(corpus / "ready-handshake/native.jsonl")[0]["raw"])
        assert read() == {"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {}}
        send({"jsonrpc": "2.0", "id": 1, "result": {
            "session_id": "sess0001", "stored_session_id": "stored",
        }})
    if case == "answered-gates":
        replay = answered_frames(corpus)
    elif case in GATE_CHECKS:
        replay = gate_frames(corpus, case)
    else:
        replay = frames(corpus / case / "native.jsonl")
    for frame in replay:
        if frame["direction"] == "host-to-gateway":
            raw = frame["raw"]
            if frame["action"] == "expect-write":
                line = sys.stdin.readline()
                if line != raw + "\n":
                    method = json.loads(line).get("method") if line else "EOF"
                    mismatch = f"expected the recorded write, got {method}"
                    Path("replay-mismatch").write_text(mismatch)
                    raise AssertionError(mismatch)
            else:
                assert read() == (json.loads(raw) if isinstance(raw, str) else raw)
        elif frame["direction"] == "gateway-to-host":
            send(frame["raw"])
        elif frame["action"] == "process-exit":
            return
    assert sys.stdin.read() == ""
    if case in REOPEN_CASES:
        Path("replay-complete").touch()


def verify_unanswered_gates(envelopes, corpus):
    expected = json.loads((corpus / "interaction-gates/expected-oap.json").read_text())
    gates = [event["payload"] for event in expected if event["type"] == "user.input.requested"]
    events = [json.loads(envelope) for envelope in envelopes]
    assert [event["type"] for event in events] == [
        "run.started", *(["user.input.requested", "run.status.updated"] * 4),
        *(["user.input.resolved"] * 4), "run.completed",
    ]
    assert [event["sequence"] for event in events] == list(range(1, 15))
    assert len({event["id"] for event in events}) == len(events)
    turns = set()
    for index, gate in enumerate(gates):
        request, waiting = events[1 + 2 * index:3 + 2 * index]
        resolved = events[9 + index]
        turn = request["turn_id"]
        turns.add(turn)
        assert request["payload"] == {**gate, "interaction_id": turn}
        assert waiting["payload"] == {
            "session_id": "session", "run_id": "run-b", "status": "waiting_for_input",
            "pending_user_input_id": turn, "updated_at_ms": waiting["timestamp_ms"] - 1,
        }
        assert resolved["turn_id"] == turn
        assert resolved["in_reply_to"] == request["id"]
        assert resolved["payload"] == {
            key: request["payload"][key] for key in
            ("interaction_id", "requested_by", "responded_by", "session_id", "run_id")
        } | {"status": "cancelled"}
    assert len(turns) == len(gates) == 4
    print(f"GATES: {len(turns)}\nCANCELLED GATES: {len(turns)}\nSEQUENCED ENVELOPES: {len(events)}")


def verify_withdrawn_gate(envelopes, _corpus):
    events = [json.loads(envelope) for envelope in envelopes]
    types = [event["type"] for event in events]
    print(f"EVENT TYPES: {', '.join(types)}")
    assert types == [
        "run.started", "user.input.requested", "run.status.updated",
        "user.input.resolved", "run.status.updated", "run.completed",
    ]
    assert [event["sequence"] for event in events] == list(range(1, 7))
    _, request, waiting, resolved, running, _ = events
    turn = request["turn_id"]
    assert waiting["payload"]["status"] == "waiting_for_input"
    assert waiting["payload"]["pending_user_input_id"] == turn
    assert resolved["turn_id"] == resolved["payload"]["interaction_id"] == turn
    assert resolved["in_reply_to"] == request["id"]
    assert resolved["payload"]["status"] == "cancelled"
    assert running["payload"]["status"] == "running"
    assert "pending_user_input_id" not in running["payload"]
    print("WITHDRAWN GATES: 1\nSEQUENCED ENVELOPES: 6")


GATE_CHECKS = {
    "unanswered-gates": verify_unanswered_gates,
    "withdrawn-gate": verify_withdrawn_gate,
}


def verify(binary, corpus, mode, case):
    fixture = "interaction-gates" if case == "answered-gates" else case
    if case in REOPEN_TIMEOUT_CASES:
        fixture = "session-reopen"
    expected_path = corpus / fixture / "expected-oap.json"
    if case in REOPEN_CASES | REOPEN_TIMEOUT_CASES:
        native = frames(corpus / fixture / "native.jsonl")
        request = next(json.loads(frame["raw"]) for frame in native if frame["action"] == "expect-write")
        stored_id = request["params"]["session_id"]
    with tempfile.TemporaryDirectory(prefix="hermes-projection-") as directory:
        root = Path(directory)
        module = root / "tui_gateway"
        module.mkdir()
        script = Path(__file__).resolve()
        (module / "entry.py").write_text(
            f"import runpy\n"
            f"from pathlib import Path\n"
            f"runpy.run_path({str(script)!r})['gateway'](Path({str(corpus)!r}), {case!r})\n"
        )
        model = ["hermes-test"] if case == "malformed-frame" else []
        if case == "answered-gates":
            answers_file = root / "answers.json"
            expected = json.loads(expected_path.read_text())
            answers_file.write_text(json.dumps([
                event["payload"]["answers"]
                for event in expected if event["type"] == "user.input.resolved"
            ]))
            model = ["", str(answers_file)]
        if case in REOPEN_CASES | REOPEN_TIMEOUT_CASES:
            model = [stored_id]
        result = subprocess.run(
            [binary, sys.executable, directory, directory, mode, *model],
            env={"PATH": os.defpath, "HOME": directory, "NODE_ENV": "test"},
            timeout=45, capture_output=True, text=True,
        )
        replay_complete = (root / "replay-complete").exists()
        resume_received = (root / "resume-received").exists()
        mismatch = root / "replay-mismatch"
        if mismatch.exists():
            print(mismatch.read_text(), file=sys.stderr)
    if case not in GATE_CHECKS and case != "answered-gates" and case not in REOPEN_CASES:
        print(result.stdout, end="")
    print(result.stderr, end="", file=sys.stderr)
    print(f"GATEWAY EXAMPLE EXIT: {result.returncode}")
    if case in REOPEN_TIMEOUT_CASES:
        assert result.returncode == 1
        assert result.stderr.strip() == "Error: DeadlineExceeded"
        assert not result.stdout
        assert resume_received == (case == "reopen-resume-timeout")
        print(f"MATCHED DEADLINE EXPIRY: {case}")
        return
    result.check_returncode()
    lines = result.stdout.split("\n")
    shutdowns = lines.count("gateway exited cleanly")
    if case in REOPEN_CASES:
        expected = expected_path.read_text()
        prefix = "state: " if case == "session-reopen" else "refusal: "
        observations = [line.removeprefix(prefix) for line in lines if line.startswith(prefix)]
        assert observations == [json.dumps(json.loads(expected), separators=(",", ":"))]
        if case == "session-reopen":
            reply = next(json.loads(frame["raw"]) for frame in native if frame["action"] == "reply")
            assert f"runtime session ID: {reply['result']['session_id']}" in lines
            assert f"stored session ID: {stored_id}" in lines
            assert shutdowns == 1
        else:
            assert not any(line.startswith(("state: ", "runtime session ID: ")) for line in lines)
        assert replay_complete
        print(f"BYTE-MATCHED SESSION RESUMES: 1\nMATCHED OUTCOME: {prefix.strip(': ')}\nGATEWAY EOF: 1")
        return
    if mode == "--run":
        envelopes = [line[10:] for line in lines if line.startswith("envelope: ")]
        if case in GATE_CHECKS:
            GATE_CHECKS[case](envelopes, corpus)
            assert shutdowns == 1
            print(f"CLEAN SHUTDOWNS: {shutdowns}")
            return
        expected = json.loads(expected_path.read_text())
        if case == "process-exit":
            assert expected[-1]["payload"]["error"]["message"] == "gateway died"
            expected[-1]["payload"]["error"]["message"] = "EOF"
        assert envelopes == [json.dumps(envelope, separators=(",", ":"), ensure_ascii=False) for envelope in expected]
        assert shutdowns == 1
        if case == "answered-gates":
            replies = sum(frame["action"] == "expect-write" for frame in answered_frames(corpus))
            print(f"BYTE-MATCHED GATE REPLIES: {replies}")
        print(f"BYTE-MATCHED ENVELOPES: {len(envelopes)}\nCLEAN SHUTDOWNS: {shutdowns}")
        return
    deltas = [json.loads(line[7:]) for line in lines if line.startswith("delta: ")]
    completed = [json.loads(line[11:]) for line in lines if line.startswith("completed: ")]
    print(f"DELTAS: {len(deltas)}\nCOMPLETIONS: {len(completed)}\nCLEAN SHUTDOWNS: {shutdowns}")
    expected = json.loads(expected_path.read_text())
    assert deltas == [entry["payload"]["part"] for entry in expected if entry["type"] == "content.delta"]
    assert completed == [
        {"text": entry["payload"]["final_response"]["content"], "usage": entry["payload"]["usage"]}
        for entry in expected if entry["type"] == "run.completed"
    ]
    assert lines.count("prompt started at native sequence 1") == shutdowns == 1


if __name__ == "__main__":
    mode = sys.argv[2] if len(sys.argv) > 2 else "--project"
    case = sys.argv[3] if len(sys.argv) > 3 else "streaming-provenance"
    verify(str(Path(sys.argv[1]).resolve()), Path("fixtures/adapters/hermes-v0.23.3").resolve(), mode, case)
