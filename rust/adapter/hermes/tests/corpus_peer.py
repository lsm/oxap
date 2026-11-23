import json
import os
import socket
import sys
import threading


def report_writes(connection):
    for line in sys.stdin:
        connection.sendall((json.dumps(line.removesuffix("\n")) + "\n").encode())
    connection.sendall(b'""\n')
    os._exit(0)


with socket.socket(socket.AF_UNIX) as connection:
    connection.connect(sys.argv[1])
    threading.Thread(target=report_writes, args=(connection,), daemon=True).start()
    with connection.makefile() as control:
        for line in control:
            operation, value = json.loads(line)
            if operation == "send":
                print(value, flush=True)
            elif operation == "disconnect":
                os.close(sys.stdout.fileno())
            else:
                raise ValueError(operation)
