import json
import os
import select
from pathlib import Path
import subprocess
import sys
import tempfile
import time


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
SHARED_BACKLOG = 300
SHARED_CANCEL_CASES = {"shared-cancel", "shared-cancel-race"}
SHARED_RESUME_CASES = {"shared-resume", "shared-resume-tiny", "shared-resume-exit"}


def shared_cancel_gateway(corpus, read, send, race):
    def event(kind, seq, **payload):
        params = {"type": kind, "session_id": "sess0001", "seq": seq}
        if payload:
            params["payload"] = payload
        send({"jsonrpc": "2.0", "method": "event", "params": params})

    def start(request_id, seq):
        assert read() == {"jsonrpc": "2.0", "id": request_id, "method": "prompt.submit",
                          "params": {"session_id": "sess0001", "text": "hello"}}
        send({"jsonrpc": "2.0", "id": request_id, "result": {"status": "streaming"}})
        event("message.start", seq)

    start(2, 1)
    pinned = [json.loads(frame["raw"]) for frame in frames(corpus / "approval-interrupt/native.jsonl")
              if frame["action"] in {"expect-write", "reply"}]
    interrupt = next(frame for frame in pinned if frame.get("method") == "session.interrupt")
    receipt = next(frame for frame in pinned if frame.get("id") == interrupt["id"] and "result" in frame)
    interrupt["params"]["session_id"] = "sess0001"
    assert read() == interrupt
    if race:
        event("message.complete", 2, status="complete", text="done")
    else:
        send(receipt)
    deadline = time.monotonic() + 10
    while not Path("release-interrupt").exists():
        assert time.monotonic() < deadline, "consumer did not check cancellation"
        assert not select.select([sys.stdin], [], [], 0.01)[0], "unexpected gateway write or EOF"
    if race:
        send({**receipt, "result": {"status": "not_interrupted", "interrupted": False}})
    else:
        event("message.complete", 2, status="interrupted")
    start(4, 3)
    event("message.complete", 4, status="complete", text="done")
    assert sys.stdin.read() == ""
    Path("replay-complete").touch()


def shared_submit_frames(corpus):
    native_path = corpus / "streaming-provenance/native.jsonl"
    replay = [frame for frame in frames(corpus / "admission-busy/native.jsonl")
              if frame["direction"] != "harness-control"]
    for index in range(2):
        for frame in frames(native_path):
            raw = frame["raw"]
            if "id" in raw:
                raw["id"] += 3 + index
            if "seq" in raw.get("params", {}):
                params = raw["params"]
                if index == 0 and params["seq"] == 2:
                    for seq in range(2, SHARED_BACKLOG + 2):
                        replay.append({**frame, "raw": {**raw, "params": {**params, "seq": seq}}})
                if index != 0 or params["seq"] >= 2:
                    params["seq"] += SHARED_BACKLOG + 7 * index
            replay.append(frame)
    return replay


def settings_gateway(corpus, case, read, send):
    def call(request_id, method, params, result=None, error=None):
        assert read() == {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        reply = {"result": result} if error is None else {"error": error}
        send({"jsonrpc": "2.0", "id": request_id, **reply})

    def setting(request_id, session_id, value, error=None):
        call(request_id, "config.set", {"session_id": session_id, "key": "reasoning", "value": value},
             {"key": "reasoning", "value": value}, error)

    if case == "native-list":
        call(1, "session.list", {"limit": 2}, {"sessions": [
            None, {"title": "missing id"}, {"id": "stored-a", "title": "Named", "started_at": 1.25},
            {"id": "stored-b", "title": "", "preview": "first words", "started_at": 2},
            {"id": "stored-c", "preview": "past the limit"},
        ]})
    elif case == "shared-settings-reopen":
        _, request, reply = frames(corpus / "session-reopen/native.jsonl")
        assert sys.stdin.readline() == request["raw"] + "\n"
        send(reply["raw"])
        setting(2, json.loads(reply["raw"])["result"]["session_id"], "none")
    else:
        call(1, "session.create", {"model": "configured", "reasoning_effort": "none"},
             {"session_id": "runtime", "stored_session_id": "stored"})
        setting(2, "runtime", "none")
        setting(3, "runtime", "high", {"code": -32000, "message": "effort rejected"})
        setting(4, "runtime", "low")
        call(5, "prompt.submit", {"session_id": "runtime", "text": "hello"}, {"status": "streaming"})
        send({"jsonrpc": "2.0", "method": "event", "params": {
            "type": "message.start", "session_id": "runtime", "seq": 1,
        }})
        deadline = time.monotonic() + 10
        while not Path("release-settings-run").exists():
            assert time.monotonic() < deadline, "consumer did not check the active update refusal"
            assert not select.select([sys.stdin], [], [], 0.01)[0], "unexpected gateway write or EOF"
        send({"jsonrpc": "2.0", "method": "event", "params": {
            "type": "message.complete", "session_id": "runtime", "seq": 2,
            "payload": {"status": "complete", "text": "done"},
        }})
    assert sys.stdin.read() == ""
    Path("replay-complete").touch()


def gateway(corpus, case):
    def read():
        return json.loads(sys.stdin.readline())

    def send(frame):
        print(frame if isinstance(frame, str) else json.dumps(frame), flush=True)

    if case in {"shared-settings", "shared-settings-reopen", "native-list"}:
        send(frames(corpus / "ready-handshake/native.jsonl")[0]["raw"])
        settings_gateway(corpus, case, read, send)
        return
    if case == "shared-open":
        send(frames(corpus / "ready-handshake/native.jsonl")[0]["raw"])
        assert read() == {"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {
            "model": "configured",
        }}
        send({"jsonrpc": "2.0", "id": 1, "result": {
            "session_id": "runtime", "stored_session_id": "stored",
        }})
        assert sys.stdin.read() == ""
        Path("replay-complete").touch()
        return
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
    shared_submit = case in {"shared-submit", "shared-resume-window"}
    if case != "malformed-frame" and case not in REOPEN_CASES:
        send(frames(corpus / "ready-handshake/native.jsonl")[0]["raw"])
        assert read() == {"jsonrpc": "2.0", "id": 1, "method": "session.create",
                          "params": {"model": "hermes-test"}
                          if shared_submit or case == "shared-input"
                          or case in SHARED_CANCEL_CASES | SHARED_RESUME_CASES else {}}
        send({"jsonrpc": "2.0", "id": 1, "result": {
            "session_id": "sess0001", "stored_session_id": "stored",
        }})
    if case in SHARED_CANCEL_CASES:
        shared_cancel_gateway(corpus, read, send, case == "shared-cancel-race")
        return
    if case in {"answered-gates", "shared-input"}:
        replay = answered_frames(corpus)
    elif case in GATE_CHECKS:
        replay = gate_frames(corpus, case)
    elif shared_submit:
        replay = shared_submit_frames(corpus)
    elif case in SHARED_RESUME_CASES:
        replay = frames(corpus / "recovery-journal/native.jsonl")
    else:
        replay = frames(corpus / case / "native.jsonl")
    pending_start = False
    shared_run = 0
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
            raw = frame["raw"]
            event_type = raw.get("params", {}).get("type") if isinstance(raw, dict) else None
            if case == "shared-resume-exit" and event_type == "message.complete":
                Path("replay-complete").touch()
                return
            if shared_submit and raw.get("id") == 2:
                Path("prompt-received").touch()
                deadline = time.monotonic() + 10
                while not Path("release-refusal").exists():
                    assert time.monotonic() < deadline, "consumer did not inspect admission"
                    assert not select.select([sys.stdin], [], [], 0.01)[0], "unexpected gateway write or EOF"
            send(raw)
            if (shared_submit or case in SHARED_RESUME_CASES) and event_type == "message.start":
                pending_start = True
        elif frame["action"] == "wait-submit" and pending_start:
            pending_start = False
            shared_run += 1
            release = Path(f"release-run-{shared_run}")
            deadline = time.monotonic() + 10
            while not release.exists():
                assert time.monotonic() < deadline, "consumer did not receive admission"
                assert not select.select([sys.stdin], [], [], 0.01)[0], "unexpected gateway write or EOF"
        elif frame["action"] == "process-exit":
            return
    assert sys.stdin.read() == ""
    if case in REOPEN_CASES | SHARED_RESUME_CASES or shared_submit or case == "shared-input":
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
    fixture = "interaction-gates" if case in {"answered-gates", "shared-input"} else case
    if case in SHARED_RESUME_CASES:
        fixture = "recovery-journal"
    if case in {"shared-submit", "shared-resume-window"}:
        fixture = "streaming-provenance"
    if case in REOPEN_TIMEOUT_CASES or case == "shared-settings-reopen":
        fixture = "session-reopen"
    expected_path = corpus / fixture / "expected-oap.json"
    if case in REOPEN_CASES | REOPEN_TIMEOUT_CASES or case == "shared-settings-reopen":
        native = frames(corpus / fixture / "native.jsonl")
        request = next(json.loads(frame["raw"]) for frame in native if frame["action"] == "expect-write")
        stored_id = request["params"]["session_id"]
    with tempfile.TemporaryDirectory(prefix="hermes-projection-") as directory:
        root = Path(directory)
        if case == "shared-resume-window":
            (root / "check-resume-window").touch()
        if case in {"shared-resume-tiny", "shared-resume-exit"}:
            (root / case).touch()
        if case == "shared-cancel-race":
            (root / "cancel-race").touch()
        module = root / "tui_gateway"
        module.mkdir()
        script = Path(__file__).resolve()
        (module / "entry.py").write_text(
            f"import runpy\n"
            f"from pathlib import Path\n"
            f"runpy.run_path({str(script)!r})['gateway'](Path({str(corpus)!r}), {case!r})\n"
        )
        model = ["hermes-test"] if case in {
            "malformed-frame", "shared-submit", "shared-resume-window",
        } | SHARED_CANCEL_CASES | SHARED_RESUME_CASES else []
        if case in {"answered-gates", "shared-input"}:
            answers_file = root / "answers.json"
            expected = json.loads(expected_path.read_text())
            answers_file.write_text(json.dumps([
                event["payload"]["answers"]
                for event in expected if event["type"] == "user.input.resolved"
            ]))
            model = ["hermes-test" if case == "shared-input" else "", str(answers_file)]
        if case in REOPEN_CASES | REOPEN_TIMEOUT_CASES or case == "shared-settings-reopen":
            model = [stored_id]
        command = [binary, sys.executable, directory, directory, mode]
        environment = {"PATH": os.defpath, "HOME": directory, "NODE_ENV": "test"}
        if case == "shared-input":
            usage = subprocess.run(command + model[:1], env=environment, timeout=5,
                                   capture_output=True, text=True)
            print(f"MISSING ANSWERS EXIT: {usage.returncode}")
            assert usage.returncode == 1 and not usage.stdout
            assert "--shared-input <model> <answers-file>" in usage.stderr, usage.stderr
        result = subprocess.run(
            command + model, env=environment,
            timeout=45, capture_output=True, text=True,
        )
        replay_complete = (root / "replay-complete").exists()
        resume_received = (root / "resume-received").exists()
        mismatch = root / "replay-mismatch"
        if mismatch.exists():
            print(mismatch.read_text(), file=sys.stderr)
    if mode == "--shared-submit":
        print("\n".join(line for line in result.stdout.splitlines() if not line.startswith("envelope: ")))
    elif case not in GATE_CHECKS and case not in {"answered-gates", "shared-input"} and case not in REOPEN_CASES:
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
    if case in {"shared-settings", "shared-settings-reopen", "native-list"}:
        assert replay_complete and shutdowns == 1
        print("GATEWAY EOF: 1")
        return
    if case in SHARED_RESUME_CASES:
        assert "LIVE REPLAY ENVELOPES: 2" in lines
        assert "COMPLETED REPLAY ENVELOPES: 1" in lines
        assert shutdowns == 1 and replay_complete
        return
    if case == "shared-input":
        actual = [json.loads(line[10:]) for line in lines if line.startswith("envelope: ")]
        expected = json.loads(expected_path.read_text())
        assert len(actual) == len(expected) == 31
        identities = {"user": "operator"}
        for observed, fixture in zip(actual, expected):
            identities[fixture["id"]] = observed["id"]
            identities[fixture["run_id"]] = observed["run_id"]
            for key in ("turn_id", "tool_call_id"):
                if key in fixture:
                    identities[fixture[key]] = observed[key]
        for observed, fixture in zip(actual, expected):
            for path, key in (("payload", "message_id"), ("final_response", "id")):
                raw = fixture["payload"] if path == "payload" else fixture["payload"].get(path, {})
                found = observed["payload"] if path == "payload" else observed["payload"].get(path, {})
                if key in raw:
                    identities[raw[key]] = found[key]
        assert len(set(identities.values())) == len(identities)

        def normalized(value, ids):
            if isinstance(value, dict):
                return {k: normalized(v, ids) for k, v in value.items() if not k.endswith("_ms")}
            if isinstance(value, list):
                return [normalized(item, ids) for item in value]
            return ids.get(value, value)

        assert normalized(expected, identities) == normalized(actual, {})
        assert replay_complete and shutdowns == 1
        replies = sum(frame["action"] == "expect-write" for frame in answered_frames(corpus))
        print(f"BYTE-MATCHED GATE REPLIES: {replies}\nMATCHED SHARED ENVELOPES: {len(actual)}\nGATEWAY EOF: 1")
        return
    if case in SHARED_CANCEL_CASES:
        assert replay_complete and shutdowns == 1
        return
    if case in REOPEN_CASES:
        expected = expected_path.read_text()
        prefix = "state: " if case == "session-reopen" else "refusal: "
        observations = [line.removeprefix(prefix) for line in lines if line.startswith(prefix)]
        assert observations == [json.dumps(json.loads(expected), separators=(",", ":"))]
        if case == "session-reopen":
            reply = next(json.loads(frame["raw"]) for frame in native if frame["action"] == "reply")
            if mode != "--shared-reopen":
                assert f"runtime session ID: {reply['result']['session_id']}" in lines
            assert f"stored session ID: {stored_id}" in lines
            assert shutdowns == 1
        else:
            assert not any(line.startswith(("state: ", "runtime session ID: ")) for line in lines)
            detail = next(line.removeprefix("detail: ") for line in lines if line.startswith("detail: "))
            assert detail == (
                "Hermes could not reload its binding: Hermes would restart the turn a crash "
                "interrupted, which a reopen must not do; open it in Hermes to settle the turn, "
                "or wait for Hermes to retire the marker"
            ), detail
        assert replay_complete
        print(f"BYTE-MATCHED SESSION RESUMES: 1\nMATCHED OUTCOME: {prefix.strip(': ')}\nGATEWAY EOF: 1")
        return
    if mode == "--shared-open":
        states = [json.loads(line[7:]) for line in lines if line.startswith("state: ")]
        assert states == [{
            "session_id": "session", "status": "idle", "current_model_id": "configured",
        }]
        assert "stored session ID: stored" in lines
        assert shutdowns == 1 and replay_complete
        print("MATCHED SHARED CREATED STATE: 1\nGATEWAY EOF: 1")
        return
    if mode == "--shared-submit":
        if case == "shared-resume-window":
            assert "REPLAY WINDOW: 256" in lines
        assert "PENDING ADMISSION: queued" in lines
        assert "REFUSALS RETURNED TO IDLE: 3" in lines
        admissions = [json.loads(line[11:]) for line in lines if line.startswith("admission: ")]
        events = [json.loads(line[10:]) for line in lines if line.startswith("envelope: ")]
        expected = json.loads(expected_path.read_text())
        first_run = expected[:1] + [expected[1]] * SHARED_BACKLOG + expected[1:]
        assert len(admissions) == 2 and len(events) == len(first_run) + len(expected)
        assert len({event["id"] for event in events}) == len(events)
        assert len({admission["run_id"] for admission in admissions}) == len(admissions)
        for admission, fixture_run in zip(admissions, (first_run, expected)):
            (message_id,) = admission["message_ids"]
            assert message_id and admission["run_id"]
            assert admission == {
                "session_id": "session", "accepted": True, "submission_id": message_id,
                "requested_delivery": "auto", "effective_delivery": "start",
                "delivery_resolution": "session_idle", "admission": "started",
                "run_id": admission["run_id"], "status": "running", "model_id": "hermes-test",
                "message_ids": [message_id],
            }
            run_events = [event for event in events if event["run_id"] == admission["run_id"]]
            assert len(run_events) == len(fixture_run)
            for sequence, (event, fixture) in enumerate(zip(run_events, fixture_run), start=1):
                assert event["run_id"] == event["payload"]["run_id"] == admission["run_id"]
                assert event["type"] == fixture["type"]
                assert event["sequence"] == sequence
                assert event["session_id"] == event["payload"]["session_id"] == "session"
                for key in ("part", "usage", "stop_reason"):
                    assert event["payload"].get(key) == fixture["payload"].get(key)
                if event["type"] == "content.delta":
                    assert event["payload"]["message_id"] == message_id
            response = run_events[-1]["payload"]["final_response"]
            assert response["content"] == expected[-1]["payload"]["final_response"]["content"]
            assert response["id"] == message_id
        assert shutdowns == 1 and replay_complete
        print(f"SHARED ADMISSIONS: {len(admissions)}\nSEQUENCED ENVELOPES: {len(events)}"
              f"\nBACKLOG DELTAS: {SHARED_BACKLOG}\nGATEWAY EOF: 1")
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
