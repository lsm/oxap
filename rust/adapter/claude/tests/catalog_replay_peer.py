#!/usr/bin/env python3
import json
import os
import socket
import sys

assert os.environ["NODE_ENV"] == "test"
assert os.path.samefile(os.environ["HOME"], os.getcwd())
with socket.socket(socket.AF_UNIX) as bridge:
    bridge.connect("wire")
    with bridge.makefile("r") as commands:
        def send(value):
            bridge.sendall((json.dumps(value) + "\n").encode())

        send(sys.argv[1:])
        for command in commands:
            op, *args = json.loads(command)
            if op == "read":
                send(json.loads(sys.stdin.readline()))
            elif op == "write":
                print(args[0], flush=True)
            elif op == "exit":
                break
            elif op == "end":
                send(sys.stdin.readline() or "eof")
                break
            else:
                raise AssertionError(op)
