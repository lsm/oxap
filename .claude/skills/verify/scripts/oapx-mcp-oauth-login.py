#!/usr/bin/env python3
"""Sign in to a fake OAuth-protected MCP source through the CLI."""
import base64
import hashlib
import http.server
import importlib.util
import json
import queue
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
import uuid


class Server(http.server.BaseHTTPRequestHandler):
    requests = []
    challenge = None
    redirect = None
    scope_mode = 'challenge'
    served = False
    refreshes = 0
    reject_tool_once = True
    reject_open = False
    redirect_open = False
    deny_refresh = False
    ping_answered = threading.Event()

    def log_message(self, *args):
        pass

    def reply(self, status, value, session=False):
        payload = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        if session:
            self.send_header('Mcp-Session-Id', 'fixture-session')
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
            method = initialize.get('method')
            auth = self.headers.get('Authorization')
            if self.served and auth and method == 'initialize' and self.redirect_open:
                self.send_response(302)
                self.send_header('Location', self.server.origin + '/leak')
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            reject = not auth or (self.served and method == 'tools/call' and self.reject_tool_once) or (self.served and method == 'initialize' and self.reject_open)
            if reject:
                if auth and method == 'tools/call':
                    Server.reject_tool_once = False
                self.send_response(401)
                if not auth:
                    challenge = f'Bearer resource_metadata="{self.server.origin}/resource-metadata"'
                    if self.scope_mode == 'challenge':
                        challenge += ', scope="tools:read tools:call"'
                    self.send_header('WWW-Authenticate', challenge)
                self.send_header('Content-Length', '0')
                self.end_headers()
                return
            assert self.served and auth == f'Bearer fixture-access-{self.refreshes}'
            if method != 'initialize':
                assert self.headers.get('Mcp-Session-Id') == 'fixture-session'
            if method is None:
                assert initialize == {'jsonrpc': '2.0', 'id': 'fixture-ping', 'result': {}}
                self.ping_answered.set()
                self.send_response(202); self.end_headers(); return
            if 'id' not in initialize:
                self.send_response(202); self.end_headers(); return
            if method == 'initialize':
                result = {'protocolVersion': '2025-03-26', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fixture', 'version': '1'}}
            elif method == 'tools/list':
                result = {'tools': [{'name': 'echo', 'description': 'Echo input', 'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string'}}}}]}
            elif method == 'tools/call':
                result = {'content': [{'type': 'text', 'text': 'echoed:' + initialize['params']['arguments']['text']}]}
            else:
                raise AssertionError(method)
            response = {'jsonrpc': '2.0', 'id': initialize['id'], 'result': result}
            if method == 'tools/list':
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.end_headers()
                self.wfile.write(b'data: {"jsonrpc":"2.0","id":"fixture-ping","method":"ping"}\n\n'); self.wfile.flush()
                assert self.ping_answered.wait(5), 'authenticated ping response missing'
                self.wfile.write(b'data: ' + json.dumps(response).encode() + b'\n\n'); self.wfile.flush()
            else:
                self.reply(200, response, session=method == 'initialize')
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
            assert form['client_id'] == ['fixture-client']
            assert form['resource'] == [self.server.origin + '/mcp']
            if form['grant_type'] == ['refresh_token']:
                assert self.served and form['refresh_token'] == [f'fixture-refresh-{self.refreshes}']
                Server.refreshes += 1
                if self.deny_refresh:
                    self.reply(400, {'error': 'invalid_grant'}); return
                self.reply(200, {'access_token': f'fixture-access-{self.refreshes}', 'refresh_token': f'fixture-refresh-{self.refreshes}', 'token_type': 'Bearer', 'expires_in': 3600})
            else:
                assert form['grant_type'] == ['authorization_code'] and form['code'] == ['fixture-code'] and form['redirect_uri'] == [self.redirect]
                verifier = form['code_verifier'][0]
                actual = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b'=').decode()
                assert actual == self.challenge
                self.reply(200, {'access_token': 'fixture-access-0', 'refresh_token': 'fixture-refresh-0', 'token_type': 'Bearer', 'expires_in': 0 if self.served else 3600})
        else:
            self.send_error(404)

    def do_DELETE(self):
        self.requests.append(('DELETE', self.path))
        assert self.served and self.headers.get('Authorization') == f'Bearer fixture-access-{self.refreshes - int(self.deny_refresh)}'
        assert self.headers.get('Mcp-Session-Id') == 'fixture-session'
        self.send_response(204)
        self.end_headers()


def main():
    binary = Path(sys.argv[1]).resolve()
    Server.scope_mode = sys.argv[2] if len(sys.argv) > 2 else 'challenge'
    Server.served = Server.scope_mode == 'served'
    if Server.served:
        Server.scope_mode = 'challenge'
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
            if Server.served:
                served_journey(command, env, root, server)
                return
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


def served_journey(command, env, root, server):
    spec = importlib.util.spec_from_file_location('stdio_journey', Path(__file__).with_name('oapx-mcp-stdio.py'))
    stdio = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(stdio)
    provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), stdio.Provider)
    thread = threading.Thread(target=provider.serve_forever)
    thread.start()
    data = Path(env['HOME']) / '.oxap'
    data.mkdir(parents=True, exist_ok=True)
    (data / 'providers.json').write_text(json.dumps({'providers': [{'id': 'synthetic', 'api': 'openai-completions', 'base_url': f'http://127.0.0.1:{provider.server_port}/v1', 'auth': 'none', 'models': ['synthetic-model']}]}))
    (data / 'config.json').write_text(json.dumps({'model': 'synthetic-model', 'provider': 'synthetic', 'api': 'openai-completions', 'workspace': str(root)}))
    (root / 'hub.json').write_text(json.dumps({'adapters': {'loop': {'type': 'oapx'}}, 'tool_sources': {'demo': {'kind': 'remote', 'protocol': 'mcp', 'endpoint': server.origin + '/mcp'}}}))
    output = root / 'evidence'; output.mkdir()
    hub = None
    try:
        hub = stdio.base.Hub(command, env, root, output, 'oauth-served', flags=('--no-save-oapx-sessions',))
        request = stdio.base.envelope('session.open.request', {'session_id': 'oauth-proof', 'tool_sources': [{'id': 'demo', 'kind': 'remote'}], 'metadata': {'oapx': {'permission_mode': 'bypass'}}})
        hub.call('open', adapter='loop', request=request)
        assert Server.refreshes == 1, f'expired login did not refresh: count={Server.refreshes}'
        catalog = hub.call('tools', session_id='oauth-proof')
        assert any(tool['name'] == 'mcp_demo_echo' for tool in catalog['payload']['tools'])
        stdio.base.submit(hub, 'oauth-proof', 'Call the echo tool with hello')
        assert 'echoed:hello' in json.dumps(stdio.Provider.requests[-1])
        assert Server.refreshes == 2, f'revoked access was not refreshed once: count={Server.refreshes}'
        hub.stop(); hub = None
        assert ('DELETE', '/mcp') in Server.requests, 'authenticated session deletion missing'
        Server.ping_answered.clear()
        hub = stdio.base.Hub(command, env, root, output, 'oauth-restart', flags=('--no-save-oapx-sessions',))
        request['payload']['session_id'] = 'oauth-restart'
        hub.call('open', adapter='loop', request=request)
        assert Server.refreshes == 2, f'restarted process did not use saved refresh: count={Server.refreshes}'
        Server.redirect_open = True
        request['payload']['session_id'] = 'redirected-source'
        try:
            hub.call('open', adapter='loop', request=request)
        except AssertionError as error:
            assert isinstance(error.args[0], dict), error
        else:
            raise AssertionError('authenticated redirect was followed')
        assert ('GET', '/leak') not in Server.requests, 'credential-bearing redirect was followed'
        Server.redirect_open = False
        Server.reject_open = True
        request['payload']['session_id'] = 'rejected-twice'
        try:
            hub.call('open', adapter='loop', request=request)
        except AssertionError as error:
            assert isinstance(error.args[0], dict), error
        else:
            raise AssertionError('repeatedly rejected access was accepted')
        assert Server.refreshes == 3, f'access rejection retried unexpectedly: count={Server.refreshes}'
        Server.deny_refresh = True
        request['payload']['session_id'] = 'revoked-refresh'
        try:
            hub.call('open', adapter='loop', request=request)
        except AssertionError as error:
            assert isinstance(error.args[0], dict), error
        else:
            raise AssertionError('revoked refresh credential was accepted')
        assert Server.refreshes == 4, f'revoked refresh was retried unexpectedly: count={Server.refreshes}'
        request['payload']['session_id'] = 'revoked-again'
        try:
            hub.call('open', adapter='loop', request=request)
        except AssertionError as error:
            assert isinstance(error.args[0], dict), error
        else:
            raise AssertionError('revoked refresh credential was accepted on retry')
        assert Server.refreshes == 4, f'revoked refresh was retried unexpectedly: count={Server.refreshes}'
        assert Server.ping_answered.is_set()
        hub.stop(); hub = None
        assert ('DELETE', '/mcp') in Server.requests, 'authenticated session deletion missing'
        print('PASS: expired credential refreshed, rejected access retried once, revoked refresh refused; refreshes=4; authenticated ping and deletion')
    finally:
        if hub:
            hub.stop()
        provider.shutdown(); provider.server_close(); thread.join()


if __name__ == '__main__':
    main()
