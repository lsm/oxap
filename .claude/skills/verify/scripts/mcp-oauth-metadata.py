#!/usr/bin/env python3
"""Check OAuth discovery against malformed and unsafe fake metadata."""
import http.server
import json
import subprocess
import sys
import threading
import time


class Server(http.server.BaseHTTPRequestHandler):
    mode = 'valid'
    requests = []

    def log_message(self, *args):
        pass

    def do_GET(self):
        self.requests.append(self.path)
        if self.mode == 'slow':
            time.sleep(6)
        origin = self.server.origin
        if self.path == '/resource':
            if self.mode == 'redirect':
                self.send_response(302)
                self.send_header('Location', origin + '/redirect-target')
                self.end_headers()
                return
            issuer = 'http://example.com' if self.mode == 'unsafe' else origin
            value = {'resource': origin + ('/other' if self.mode == 'resource' else '/mcp'), 'authorization_servers': [issuer]}
        elif self.path == '/.well-known/oauth-authorization-server':
            value = {'issuer': origin + '/wrong' if self.mode == 'issuer' else origin,
                     'authorization_endpoint': origin + '/authorize', 'token_endpoint': origin + '/token'}
            if self.mode == 'oversized':
                value['padding'] = 'x' * (256 * 1024 + 1)
        else:
            self.send_error(404)
            return
        payload = json.dumps(value).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            pass


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
server.origin = f'http://127.0.0.1:{server.server_port}'
thread = threading.Thread(target=server.serve_forever)
thread.start()
try:
    for mode, expected, count in [('valid', 0, 2), ('resource', 1, 1), ('issuer', 1, 2), ('unsafe', 1, 1), ('redirect', 1, 1), ('oversized', 1, 2), ('slow', 1, 1)]:
        Server.mode = mode
        Server.requests = []
        env = {'PATH': '/usr/bin:/bin', 'NODE_ENV': 'test', 'MCP_TEST_URL': server.origin + '/mcp',
               'MCP_TEST_CHALLENGE': f'Bearer realm="one,two", resource_metadata="{server.origin}/resource"'}
        result = subprocess.run([sys.argv[1]], env=env, capture_output=True, text=True, timeout=15)
        assert result.returncode == expected and len(Server.requests) == count, (mode, result.returncode, result.stderr, Server.requests)
        print(f'{mode}: exit={result.returncode} requests={len(Server.requests)}')
    print('PASS: discovery rejects invalid metadata, unsafe URLs, redirects, oversized metadata, and timeout')
finally:
    server.shutdown()
    server.server_close()
    thread.join()
