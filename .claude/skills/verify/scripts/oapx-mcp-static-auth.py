#!/usr/bin/env python3
"""Observe configured static MCP headers and bearer authentication."""
import http.server
import importlib.util
import json
from pathlib import Path
import shutil
import sys
import tempfile
import threading
import uuid

spec = importlib.util.spec_from_file_location('http_journey', Path(__file__).with_name('oapx-mcp-http.py'))
http_journey = importlib.util.module_from_spec(spec)
spec.loader.exec_module(http_journey)
stdio = http_journey.stdio
base = stdio.base


class Server(http.server.BaseHTTPRequestHandler):
    requests = 0
    ping_answered = threading.Event()
    redirect = False
    require_headers = True
    require_auth = True

    def log_message(self, *args):
        pass

    def do_POST(self):
        Server.requests += 1
        self.check_headers()
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method = request.get('method')
        if self.redirect and method == 'initialize':
            self.send_response(302)
            self.send_header('Location', f'http://127.0.0.1:{self.server.server_port}/leak')
            self.end_headers(); return
        if method != 'initialize':
            assert self.headers.get('Mcp-Session-Id') == 'fixture-session'
            assert self.headers.get('MCP-Protocol-Version') == '2025-03-26'
        if method is None:
            assert request == {'jsonrpc': '2.0', 'id': 'fixture-ping', 'result': {}}
            self.ping_answered.set()
            self.send_response(202); self.end_headers(); return
        if 'id' not in request:
            self.send_response(202); self.end_headers(); return
        if method == 'initialize':
            result = {'protocolVersion': '2025-03-26', 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fixture', 'version': '1'}}
        elif method == 'tools/list':
            result = {'tools': [{'name': 'echo', 'description': 'Echo input', 'inputSchema': {'type': 'object', 'properties': {'text': {'type': 'string'}}}}]}
        elif method == 'tools/call':
            result = {'content': [{'type': 'text', 'text': 'echoed:' + request['params']['arguments']['text']}]}
        else:
            raise AssertionError(method)
        payload = json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}).encode()
        self.send_response(200)
        if method == 'initialize': self.send_header('Mcp-Session-Id', 'fixture-session')
        if method == 'tools/list':
            self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
            self.wfile.write(b'data: {"jsonrpc":"2.0","id":"fixture-ping","method":"ping"}\n\n'); self.wfile.flush()
            assert self.ping_answered.wait(5), 'authenticated ping response missing'
            self.wfile.write(b'data: ' + payload + b'\n\n'); self.wfile.flush(); return
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload)

    def do_DELETE(self):
        Server.requests += 1
        self.check_headers()
        assert self.headers.get('Mcp-Session-Id') == 'fixture-session'
        self.send_response(204); self.end_headers()

    def do_GET(self):
        Server.requests += 1
        self.send_error(500)

    def check_headers(self):
        assert self.headers.get('X-Literal') == ('literal-value' if self.require_headers else None)
        assert self.headers.get('X-From-Env') == ('environment-value' if self.require_headers else None)
        assert self.headers.get('Authorization') == ('Bearer static-token' if self.require_auth else None)


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    for mode in ('env', 'literal', 'headers-only', 'token-only', 'null', 'redirect', 'missing-header', 'missing-token', 'empty-header', 'empty-token', 'invalid-value', 'auth-conflict'):
        with tempfile.TemporaryDirectory(prefix='oap-mcp-static-') as temporary:
            root = Path(temporary); home = root / 'home'; data = home / '.oapx'; data.mkdir(parents=True)
            binary = root / 'oxap'; shutil.copy2(base.ROOT / 'zig/zig-out/bin/oxap', binary)
            provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), stdio.Provider)
            mcp = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Server)
            threads = [threading.Thread(target=s.serve_forever) for s in (provider, mcp)]
            for thread in threads: thread.start()
            (data / 'providers.json').write_text(json.dumps({'providers': [{'id': 'synthetic', 'api': 'openai-completions', 'base_url': f'http://127.0.0.1:{provider.server_port}/v1', 'auth': 'none', 'models': ['synthetic-model']}]}))
            (data / 'config.json').write_text(json.dumps({'model': 'synthetic-model', 'provider': 'synthetic', 'api': 'openai-completions', 'workspace': str(root)}))
            source = {'kind': 'remote', 'protocol': 'mcp', 'endpoint': f'http://127.0.0.1:{mcp.server_port}/mcp', 'headers': {'X-Literal': 'literal-value', 'X-From-Env': {'env': 'MCP_HEADER_VALUE'}}, 'auth': 'static-token' if mode == 'literal' else {'env': 'MCP_TOKEN_VALUE'}}
            if mode == 'headers-only': del source['auth']
            if mode == 'token-only': del source['headers']
            if mode == 'null': source.update(headers=None, auth=None)
            if mode == 'invalid-value': source['headers']['X-Injected'] = 'bad\r\nInjected: value'
            if mode == 'auth-conflict': source['headers']['authorization'] = 'other'
            (root / 'hub.json').write_text(json.dumps({'adapters': {'loop': {'type': 'oapx'}}, 'tool_sources': {'demo': source}}))
            env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'TMPDIR': str(root), 'NODE_ENV': 'test', 'OAPX_KEYCHAIN_SERVICE': 'oap-verify-' + str(uuid.uuid4())}
            if mode != 'missing-header': env['MCP_HEADER_VALUE'] = 'environment-value'
            if mode != 'missing-token': env['MCP_TOKEN_VALUE'] = 'static-token'
            if mode == 'empty-header': env['MCP_HEADER_VALUE'] = ''
            if mode == 'empty-token': env['MCP_TOKEN_VALUE'] = ''
            command = [str(binary)]
            if sys.platform == 'darwin':
                profile = root / 'isolation.sb'
                profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
                command = ['/usr/bin/sandbox-exec', '-f', str(profile), str(binary)]
            Server.requests = 0
            Server.redirect = mode == 'redirect'
            Server.require_headers = mode not in ('token-only', 'null')
            Server.require_auth = mode not in ('headers-only', 'null')
            Server.ping_answered.clear()
            stdio.Provider.requests = []
            hub = None
            try:
                hub = base.Hub(command, env, root, output, mode, flags=('--no-save-oapx-sessions',))
                request = base.envelope('session.open.request', {'session_id': 'static-proof', 'tool_sources': [{'id': 'demo', 'kind': 'remote'}], 'metadata': {'oapx': {'permission_mode': 'bypass'}}})
                if mode.startswith(('missing-', 'empty-')) or mode in ('invalid-value', 'auth-conflict'):
                    try:
                        hub.call('open', adapter='loop', request=request)
                    except AssertionError:
                        pass
                    else:
                        raise AssertionError('missing environment variable was accepted')
                    assert hub.process.wait(timeout=5) != 0
                    expected_name = {'invalid-value': 'invalid header value', 'auth-conflict': 'conflicts with the authorization header'}.get(mode, 'MCP_HEADER_VALUE' if mode.endswith('-header') else 'MCP_TOKEN_VALUE')
                    assert expected_name in (output / (mode + '-stderr.txt')).read_text()
                    assert Server.requests == 0, f'requests={Server.requests}'
                    print(f'PASS: {mode} refused; requests=0')
                    continue
                if mode == 'redirect':
                    try:
                        hub.call('open', adapter='loop', request=request)
                    except AssertionError as error:
                        assert isinstance(error.args[0], dict), error
                    else:
                        raise AssertionError('authenticated redirect was followed')
                    assert Server.requests == 1, f'requests={Server.requests}'
                    print('PASS: authenticated redirect refused; requests=1')
                    continue
                hub.call('open', adapter='loop', request=request)
                catalog = hub.call('tools', session_id='static-proof')
                assert any(tool['name'] == 'mcp_demo_echo' for tool in catalog['payload']['tools'])
                base.submit(hub, 'static-proof', 'Call the echo tool with hello')
                assert 'echoed:hello' in json.dumps(stdio.Provider.requests[-1])
                assert Server.ping_answered.is_set()
                hub.stop(); hub = None
                assert Server.requests == 6, f'requests={Server.requests}'
                print(f'PASS: {mode} configuration reached MCP tool, ping and deletion; requests={Server.requests}')
            finally:
                try:
                    if hub:
                        try:
                            hub.stop()
                        except AssertionError:
                            if not (mode.startswith(('missing-', 'empty-')) or mode in ('invalid-value', 'auth-conflict')): raise
                finally:
                    for server in (provider, mcp): server.shutdown(); server.server_close()
                    for thread in threads: thread.join()
                    for path in (output / (mode + '.jsonl'), output / (mode + '-stderr.txt')):
                        text = path.read_text()
                        assert 'environment-value' not in text and 'static-token' not in text, f'credential appeared in {path.name}'


if __name__ == '__main__':
    main()
