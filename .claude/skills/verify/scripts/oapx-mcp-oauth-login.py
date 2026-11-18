#!/usr/bin/env python3
"""Sign in to a fake OAuth-protected MCP source through the CLI."""
import base64
import hashlib
import http.server
import json
import queue
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import urllib.parse
import urllib.request
import uuid


class Server(http.server.BaseHTTPRequestHandler):
    requests = []
    challenge = None
    redirect = None
    scope_mode = 'challenge'

    def log_message(self, *args):
        pass

    def reply(self, status, value):
        payload = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.requests.append(('GET', self.path.split('?')[0]))
        origin = self.server.origin
        if self.path == '/resource-metadata':
            metadata = {'resource': origin + '/mcp', 'authorization_servers': [origin]}
            if self.scope_mode != 'none':
                metadata['scopes_supported'] = ['metadata-scope']
            self.reply(200, metadata)
        elif self.path == '/.well-known/oauth-authorization-server':
            self.reply(200, {'issuer': origin, 'authorization_endpoint': origin + '/authorize', 'token_endpoint': origin + '/token', 'registration_endpoint': origin + '/register', 'code_challenge_methods_supported': ['S256']})
        elif self.path.startswith('/authorize?'):
            params = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            assert params['response_type'] == ['code'] and params['client_id'] == ['fixture-client']
            assert params['code_challenge_method'] == ['S256'] and params['resource'] == [origin + '/mcp']
            if self.scope_mode == 'none':
                assert 'scope' not in params
            else:
                assert params['scope'] == (['tools:read tools:call'] if self.scope_mode == 'challenge' else ['metadata-scope'])
            assert params['redirect_uri'] == [self.redirect]
            Server.challenge = params['code_challenge'][0]
            destination = self.redirect + '?' + urllib.parse.urlencode({'code': 'fixture-code', 'state': params['state'][0]})
            self.send_response(302)
            self.send_header('Location', destination)
            self.end_headers()
        else:
            self.send_error(404)

    def do_POST(self):
        self.requests.append(('POST', self.path))
        payload = self.rfile.read(int(self.headers.get('Content-Length', '0')))
        if self.path == '/mcp':
            initialize = json.loads(payload)
            assert initialize['method'] == 'initialize' and initialize['params']['protocolVersion'] == '2025-03-26'
            self.send_response(401)
            challenge = f'Bearer resource_metadata="{self.server.origin}/resource-metadata"'
            if self.scope_mode == 'challenge':
                challenge += ', scope="tools:read tools:call"'
            self.send_header('WWW-Authenticate', challenge)
            self.send_header('Content-Length', '0')
            self.end_headers()
        elif self.path == '/register':
            metadata = json.loads(payload)
            assert metadata['token_endpoint_auth_method'] == 'none'
            assert metadata['response_types'] == ['code']
            assert metadata['grant_types'] == ['authorization_code', 'refresh_token']
            Server.redirect = metadata['redirect_uris'][0]
            parsed = urllib.parse.urlsplit(self.redirect)
            assert parsed.scheme == 'http' and parsed.hostname in ('localhost', '127.0.0.1') and parsed.port
            self.reply(201, {'client_id': 'fixture-client', 'token_endpoint_auth_method': 'none'})
        elif self.path == '/token':
            form = urllib.parse.parse_qs(payload.decode())
            assert form['grant_type'] == ['authorization_code'] and form['code'] == ['fixture-code']
            assert form['client_id'] == ['fixture-client'] and form['redirect_uri'] == [self.redirect]
            assert form['resource'] == [self.server.origin + '/mcp']
            verifier = form['code_verifier'][0]
            actual = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b'=').decode()
            assert actual == self.challenge
            self.reply(200, {'access_token': 'fixture-access', 'refresh_token': 'fixture-refresh', 'token_type': 'Bearer', 'expires_in': 3600})
        else:
            self.send_error(404)


def main():
    binary = Path(sys.argv[1]).resolve()
    Server.scope_mode = sys.argv[2] if len(sys.argv) > 2 else 'challenge'
    assert Server.scope_mode in ('challenge', 'metadata', 'none')
    with tempfile.TemporaryDirectory(prefix='oap-mcp-login-') as temporary:
        root = Path(temporary)
        home = root / 'home'
        home.mkdir()
        program = root / 'oxap'
        shutil.copy2(binary, program)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
        server.origin = f'http://127.0.0.1:{server.server_port}'
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'TMPDIR': str(root), 'NODE_ENV': 'test', 'OAPX_KEYCHAIN_SERVICE': 'oap-verify-' + str(uuid.uuid4())}
        command = [str(program)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(allow network-bind (local ip "localhost:*"))\n(allow network-inbound (local ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec', '-f', str(profile), str(program)]
        process = None
        try:
            process = subprocess.Popen(command + ['auth', 'mcp-login', '--endpoint', server.origin + '/mcp'], env=env, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            lines = queue.Queue()
            def read():
                for line in process.stdout:
                    lines.put(line.strip())
                lines.put(None)
            reader = threading.Thread(target=read)
            reader.start()
            errors = []
            while True:
                line = lines.get(timeout=30)
                assert line is not None, f'login exited before showing authorization URL: exit={process.poll()} errors={errors} requests={Server.requests}'
                errors.extend(re.findall(r'error: ([A-Za-z][A-Za-z0-9]+)', line))
                if line.startswith(server.origin + '/authorize?'):
                    query = urllib.parse.parse_qs(urllib.parse.urlsplit(line).query)
                    forged = query['redirect_uri'][0] + '?' + urllib.parse.urlencode({'code': 'forged', 'state': 'wrong'})
                    try:
                        urllib.request.urlopen(forged, timeout=10)
                        raise AssertionError('incorrect state accepted')
                    except urllib.error.HTTPError as error:
                        assert error.code == 400
                    with urllib.request.urlopen(line, timeout=10) as browser:
                        assert browser.status == 200
                    break
            code = process.wait(timeout=30)
            reader.join()
            assert code == 0, f'login exit={code}'
            status = subprocess.run(command + ['auth', 'mcp-status', '--endpoint', server.origin + '/mcp'], env=env, cwd=root, capture_output=True, text=True, timeout=15)
            assert status.returncode == 0 and status.stdout.strip() == 'authenticated (refreshable)', (status.returncode, status.stdout, status.stderr)
            expected = [('POST', '/mcp'), ('GET', '/resource-metadata'), ('GET', '/.well-known/oauth-authorization-server'), ('POST', '/register'), ('GET', '/authorize'), ('POST', '/token')]
            assert Server.requests == expected, Server.requests
            print(f'PASS: browser login and persisted status; scope={Server.scope_mode}; requests={len(Server.requests)}; login_exit={code}; status_exit={status.returncode}')
        finally:
            if process and process.poll() is None:
                process.kill()
                process.wait()
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    main()
