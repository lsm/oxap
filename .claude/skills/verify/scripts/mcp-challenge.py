#!/usr/bin/env python3
"""Exercise the MCP HTTP transport against a server issuing a 401 challenge."""
import http.server
import subprocess
import sys
import threading


class Server(http.server.BaseHTTPRequestHandler):
    requests = 0

    def log_message(self, *args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        Server.requests += 1
        self.send_response(401)
        self.send_header('WWW-Authenticate', 'Bearer resource_metadata="http://127.0.0.1/metadata"')
        self.send_header('Content-Length', '0')
        self.end_headers()


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
thread = threading.Thread(target=server.serve_forever)
thread.start()
try:
    env = {'PATH': '/usr/bin:/bin', 'NODE_ENV': 'test',
           'MCP_TEST_URL': f'http://127.0.0.1:{server.server_port}/mcp'}
    result = subprocess.run([sys.argv[1]], env=env, timeout=15)
    print(f'probe_exit={result.returncode} requests={Server.requests}', flush=True)
    if result.returncode or Server.requests != 1:
        sys.exit(1)
finally:
    server.shutdown()
    server.server_close()
    thread.join()
