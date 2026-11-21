#!/bin/sh
cd "$(dirname "$0")" || exit 1
exec python3 -c '
import json, sys
source, sink = sys.stdin.buffer, sys.stdout.buffer
log = open("stdin.log", "a")
def read():
    length = None
    while True:
        line = source.readline()
        if not line:
            sys.exit(0)
        if line == b"\r\n":
            break
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"content-length":
            length = int(value.strip())
    body = source.read(length)
    log.write(body.decode() + "\n")
    log.flush()
    return json.loads(body)
def write(body):
    data = json.dumps(body).encode()
    sink.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sink.flush()
def reply(message, result):
    write({"jsonrpc": "2.0", "id": message["id"], "result": result})
def event(kind, data):
    write({"jsonrpc": "2.0", "method": "session.event", "params": {"sessionId": "ses", "event": {"type": kind, "data": data}}})
while True:
    message = read()
    method = message.get("method")
    if method == "ping":
        reply(message, {"protocolVersion": 4})
    elif method == "status.get":
        reply(message, {"version": "1.0.114"})
    elif method == "session.create":
        reply(message, {"sessionId": "ses"})
    elif method == "session.send":
        reply(message, {"messageId": "m1"})
        event("user.message", {"messageId": "m1", "interactionId": "i1", "content": message["params"]["prompt"]})
        event("assistant.message_delta", {"interactionId": "i1", "deltaContent": "Hel"})
        event("assistant.message_delta", {"interactionId": "i1", "deltaContent": "lo"})
        event("assistant.usage", {"interactionId": "i1", "inputTokens": 3, "outputTokens": 2})
        event("session.idle", {})
'
