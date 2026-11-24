import json
import sys
from pathlib import Path


THOUGHT = "Check both inputs."
REPLY = "Both inputs checked."
TOOL_ARGS = {"path": "fixture.txt"}
TOOL_RESULT = {"text": "fixture read"}
SESSION = "s1"
PACKED = [f"Reasoning fragment {index}. " for index in range(624)]


def peer(kind):
    framed = kind == "copilot"
    source, sink = sys.stdin.buffer, sys.stdout.buffer

    def read():
        header = source.readline()
        if not header:
            raise EOFError
        if not framed:
            return json.loads(header)
        length = int(header.split(b":", 1)[1])
        assert source.readline() == b"\r\n"
        return json.loads(source.read(length))

    def emit(**frame):
        data = json.dumps(frame).encode()
        sink.write(b"Content-Length: %d\r\n\r\n" % len(data) + data if framed else data + b"\n")
        sink.flush()

    def request(method):
        frame = read()
        assert frame.get("method", frame.get("type")) == method, frame
        return frame

    rpc_version = {} if kind == "codex" else {"jsonrpc": "2.0"}

    def rpc(frame, **result):
        emit(id=frame["id"], result=result, **rpc_version)

    def notify(method, **params):
        emit(method=method, params=params, **rpc_version)

    if kind == "claude":
        init = request("control_request")
        emit(type="control_response", response={"subtype": "success",
             "request_id": init["request_id"], "response": {}})
        turn = request("user")
        emit(type="system", subtype="init", session_id=SESSION, tools=["Read"], model="fake", uuid="init")
        fields = {"session_id": SESSION, "parent_tool_use_id": None, "user_message_uuid": turn["uuid"]}
        emit(type="stream_event", event={"type": "message_start"}, uuid="start", **fields)
        for index, (delta, field, text) in enumerate((
            ("thinking_delta", "thinking", THOUGHT),
            ("text_delta", "text", REPLY[:5]), ("text_delta", "text", REPLY[5:]),
        )):
            emit(type="stream_event", event={"type": "content_block_delta", "index": index,
                 "delta": {"type": delta, field: text}}, uuid=f"d{index}", **fields)
        emit(type="assistant", message={"id": "msg_1", "model": "fake", "content": [
             {"type": "tool_use", "id": "call", "name": "Read", "input": TOOL_ARGS}]}, uuid="call", **fields)
        emit(type="user", message={"role": "user", "content": [
             {"type": "tool_result", "tool_use_id": "call", "content": TOOL_RESULT["text"],
              "is_error": False}]}, uuid="tool-result", **fields)
        emit(type="result", subtype="success", terminal_reason="completed", stop_reason="end_turn",
             result=REPLY, queued_turn_count=0, uuid="result", **fields)

    elif kind == "codex":
        rpc(request("initialize"), userAgent="fixture", codexHome="/codex", platformFamily="unix", platformOs="linux")
        request("initialized")
        first = read()
        if first.get("method") == "model/list":
            rpc(first, data=[], nextCursor=None)
            return
        assert first.get("method") == "thread/start", first
        rpc(first, thread={"id": SESSION})
        turn = {"id": "turn-1", "status": "inProgress"}
        rpc(request("turn/start"), turn=turn)
        fields = {"threadId": SESSION, "turnId": turn["id"]}
        notify("turn/started", threadId=SESSION, turn=turn)
        for delta in (THOUGHT[:6], THOUGHT[6:]):
            notify("item/reasoning/summaryTextDelta", itemId="thought", summaryIndex=0, delta=delta, **fields)
        for text in (REPLY[:5], REPLY[5:]):
            notify("item/agentMessage/delta", itemId="message", delta=text, **fields)
        notify("item/completed", item={"type": "agentMessage", "id": "message", "text": REPLY}, **fields)
        tool = {"type": "commandExecution", "id": "call", "command": "printf fixture", "cwd": "/",
                "status": "inProgress"}
        notify("item/started", item=tool, **fields)
        notify("item/completed", item={**tool, "status": "completed",
               "aggregatedOutput": TOOL_RESULT["text"], "exitCode": 0}, **fields)
        notify("turn/completed", threadId=SESSION, turn={**turn, "status": "completed"})

    elif kind == "acp":
        rpc(request("initialize"), agentCapabilities={}, protocolVersion=1)
        rpc(request("session/new"), sessionId=SESSION)
        prompt = request("session/prompt")

        def update(update_kind, **fields):
            notify("session/update", sessionId=SESSION, update={"sessionUpdate": update_kind, **fields})

        update("agent_thought_chunk", content={"type": "text", "text": THOUGHT})
        for text in (REPLY[:5], REPLY[5:]):
            update("agent_message_chunk", content={"type": "text", "text": text})
        update("tool_call", toolCallId="call", kind="read", title="Read file", rawInput=TOOL_ARGS,
               status="pending")
        update("tool_call_update", toolCallId="call", status="completed", rawOutput=TOOL_RESULT)
        rpc(prompt, stopReason="end_turn")

    elif kind == "copilot":
        catalog = json.loads((Path(__file__).resolve().parents[4] / "harnesses/copilot-cli.json").read_text())
        version = next(item["endpoint_version"] for item in catalog["versions"] if item["status"] == "current")
        rpc(request("ping"), protocolVersion=4)
        rpc(request("status.get"), version=version)
        rpc(request("session.create"), sessionId=SESSION)
        prompt = request("session.send")
        rpc(prompt, messageId="m1")

        def event(event_kind, **fields):
            notify("session.event", sessionId=SESSION, event={"type": event_kind, "data": fields})

        event("user.message", messageId="m1", interactionId="i1", content=prompt["params"]["prompt"])
        event("assistant.reasoning_delta", reasoningId="thought", deltaContent=THOUGHT)
        for text in (REPLY[:5], REPLY[5:]):
            event("assistant.message_delta", interactionId="i1", deltaContent=text)
        event("tool.execution_start", toolCallId="call", toolName="read", arguments=TOOL_ARGS)
        event("tool.execution_complete", toolCallId="call", success=True, result=TOOL_RESULT)
        event("assistant.message", interactionId="i1", messageId="assistant", content=REPLY, phase="final_answer")
        event("session.idle")

    elif kind == "hermes":
        notify("event", type="gateway.ready", payload={"skin": {}, "change_events": True,
               "replay_epoch": "e3b0c44298fc1c149afbf4c8996fb924"})
        rpc(request("session.create"), session_id=SESSION, stored_session_id="fixture", message_count=0, info={})
        rpc(request("prompt.submit"), status="streaming")
        for index, (event_kind, payload) in enumerate((
            ("message.start", {}), ("reasoning.delta", {"text": THOUGHT}),
            ("message.delta", {"text": REPLY[:5]}), ("message.delta", {"text": REPLY[5:]}),
            ("tool.start", {"tool_id": "call", "name": "read", "args": TOOL_ARGS}),
            ("tool.complete", {"tool_id": "call", "result": TOOL_RESULT}),
            ("message.complete", {"text": REPLY, "status": "complete"}),
        ), 1):
            notify("event", type=event_kind, session_id=SESSION, seq=index, payload=payload)

    elif kind == "pi":
        def response(frame, **fields):
            emit(type="response", id=frame["id"], command=frame["type"], success=True, **fields)

        def state(frame, count):
            response(frame, data={"thinkingLevel": "off", "steeringMode": "all",
                     "followUpMode": "one-at-a-time", "messageCount": count, "pendingMessageCount": 0,
                     "sessionId": SESSION, "isStreaming": False, "model": {"id": "model", "provider": "fixture"}})

        frame = read()
        while frame["type"] == "get_state":
            state(frame, 0)
            frame = read()
        assert frame["type"] == "prompt", frame
        response(frame)
        assistant = {"role": "assistant", "content": [{"type": "thinking", "thinking": THOUGHT},
                     {"type": "text", "text": REPLY}], "api": "messages", "provider": "fixture",
                     "model": "model", "usage": {"input": 1, "output": 1, "totalTokens": 2},
                     "stopReason": "stop", "timestamp": 1}
        emit(type="agent_start")
        emit(type="turn_start")
        emit(type="message_start", message={**assistant, "content": []})
        for event_kind, index, text in (
            ("thinking_delta", 0, THOUGHT), ("text_delta", 1, REPLY[:5]), ("text_delta", 1, REPLY[5:]),
        ):
            emit(type="message_update", assistantMessageEvent={"type": event_kind, "contentIndex": index,
                 "delta": text}, usage={})
        emit(type="tool_execution_start", toolCallId="call", toolName="read", args=TOOL_ARGS)
        emit(type="tool_execution_end", toolCallId="call", toolName="read", result=TOOL_RESULT, isError=False)
        emit(type="message_end", message=assistant)
        emit(type="turn_end", message=assistant, toolResults=[])
        emit(type="agent_end", messages=[assistant], willRetry=False)
        emit(type="agent_settled")

    elif kind == "deepseek":
        count = 0

        def event(event_kind, data, **fields):
            nonlocal count
            count += 1
            notify("session.event", sessionId=native, event={
                "seq": count, "time": count, "type": event_kind, "data": data, **fields})

        rpc(request("initialize"), serverInfo={"name": "deepseek-harness-sdk-runtime", "version": "0.0.1"})
        prompt = request("session/prompt")
        native = prompt["params"]["sessionId"]
        user = {"id": "user-1", "role": "user", "source": {"kind": "user"},
                "content": prompt["params"]["contentBlocks"]}
        event("agent/inbox/spliced", {"inserted": [user], "start": 0, "target": "next-turn"})
        rpc(prompt, messageId=user["id"])
        notify("session.status", sessionId=native, status="running")
        event("turn/start", {"turn": 1})
        step = {"step": 1, "turn": 1}
        event("step/start", step)
        event("user/message", user, surfaceOp="append")
        parts = (("reasoning", PACKED), ("text", [REPLY[:5], REPLY[5:]]))
        event("assistant/message", {**step, "message": {
            "id": "assistant-1", "role": "assistant",
            "source": {"kind": "model", "model": "deepseek-v4-flash", "provider": "deepseek-official"},
            "content": [{"type": part, "text": "".join(texts)} for part, texts in parts],
        }, "stream": [{"type": f"{part}-chunks", "time0": 5, "index": index,
                       "dt": [0] * (len(texts) - 1), "texts": texts}
                      for index, (part, texts) in enumerate(parts)]}, surfaceOp="append")
        event("tool/call", {**step, "callId": "call", "name": "read", "arguments": json.dumps(TOOL_ARGS)})
        event("tool/result", {**step, "message": {
            "id": "tool-result", "role": "tool", "toolCallId": "call", "isError": False,
            "source": {"kind": "tool", "callId": "call"},
            "content": [{"type": "text", "text": TOOL_RESULT["text"]}],
        }}, surfaceOp="append")
        event("step/end", step)
        event("turn/end", {"reason": {"kind": "completed"}, "turn": 1})
        notify("session.status", sessionId=native, status="idle")
        Path("native-turn-ended").touch()
    else:
        raise ValueError(kind)

    while True:
        frame = read()
        if kind == "deepseek" and frame["method"] == "shutdown":
            rpc(frame)
            return
        if kind == "pi" and frame["type"] == "get_state":
            state(frame, 2)
        else:
            raise AssertionError(frame)


if __name__ == "__main__":
    try:
        peer(sys.argv[1])
    except EOFError:
        pass
