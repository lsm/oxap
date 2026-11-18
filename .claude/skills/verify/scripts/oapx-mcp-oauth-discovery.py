#!/usr/bin/env python3
"""Observe OAuth metadata discovery from a protected served MCP source."""
import http.server
import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tempfile
import threading
import uuid

spec = importlib.util.spec_from_file_location('stdio_journey', Path(__file__).with_name('oapx-mcp-stdio.py'))
stdio = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stdio)
base = stdio.base


class OAuth(http.server.BaseHTTPRequestHandler):
    requests = []

    def log_message(self, *args):
        pass

    def reply(self, value):
        payload = json.dumps(value).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', '0')))
        self.requests.append(('POST', self.path))
        self.send_response(401)
        self.send_header('WWW-Authenticate', f'Bearer resource_metadata="{self.server.origin}/resource-metadata"')
        self.send_header('Content-Length', '0')
        self.end_headers()

    def do_GET(self):
        self.requests.append(('GET', self.path))
        origin = self.server.origin
        if self.path == '/resource-metadata':
            self.reply({'resource': origin + '/mcp', 'authorization_servers': [origin]})
        elif self.path == '/.well-known/oauth-authorization-server':
            self.reply({'issuer': origin, 'authorization_endpoint': origin + '/authorize', 'token_endpoint': origin + '/token', 'registration_endpoint': origin + '/register', 'code_challenge_methods_supported': ['S256']})
        else:
            self.send_error(404)


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-mcp-oauth-') as temporary:
        root = Path(temporary).resolve()
        home = root / 'home'
        data = home / '.oapx'
        data.mkdir(parents=True)
        binary = root / 'oxap'
        shutil.copy2(base.ROOT / 'zig/zig-out/bin/oxap', binary)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), OAuth)
        server.origin = f'http://127.0.0.1:{server.server_port}'
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        (data / 'config.json').write_text(json.dumps({'model': 'synthetic-model', 'provider': 'synthetic', 'api': 'openai-completions', 'workspace': str(root)}))
        (data / 'providers.json').write_text(json.dumps({'providers': [{'id': 'synthetic', 'api': 'openai-completions', 'base_url': server.origin, 'auth': 'none', 'models': ['synthetic-model']}]}))
        (root / 'hub.json').write_text(json.dumps({'adapters': {'loop': {'type': 'oapx'}}, 'tool_sources': {'demo': {'kind': 'remote', 'protocol': 'mcp', 'endpoint': server.origin + '/mcp'}}}))
        environment = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'TMPDIR': str(root), 'NODE_ENV': 'test', 'OAPX_KEYCHAIN_SERVICE': 'oap-verify-' + str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec', '-f', str(profile), str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'oauth', flags=('--no-save-oapx-sessions',))
            request = base.envelope('session.open.request', {'session_id': 'oauth-proof', 'tool_sources': [{'id': 'demo', 'kind': 'remote'}]})
            try:
                hub.call('open', adapter='loop', request=request)
            except AssertionError:
                pass
            else:
                raise AssertionError('protected source unexpectedly opened without login')
            expected = [('POST', '/mcp'), ('GET', '/resource-metadata'), ('GET', '/.well-known/oauth-authorization-server')]
            assert OAuth.requests == expected, f'expected {expected}; got {OAuth.requests}'
            print(f'PASS: discovered resource and authorization metadata; requests={len(OAuth.requests)}')
        finally:
            if hub:
                hub.stop()
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    main()
