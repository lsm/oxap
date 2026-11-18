#!/usr/bin/env python3
"""Observe a served oapx session call a synthetic Streamable HTTP MCP tool."""
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import threading
import uuid

spec = importlib.util.spec_from_file_location('stdio_journey', Path(__file__).with_name('oapx-mcp-stdio.py'))
stdio = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stdio)
base = stdio.base

class Mcp(http.server.BaseHTTPRequestHandler):
    methods = []
    ping_answered = threading.Event()
    stateful = os.environ.get('OAP_VERIFY_MCP_STATELESS') != '1'
    def log_message(self, *args):
        pass
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        if 'method' not in request:
            assert request == {'jsonrpc':'2.0','id':'ping-1','result':{}}
            self.ping_answered.set()
            self.send_response(202); self.end_headers(); return
        method = request['method']
        self.methods.append(method)
        assert 'application/json' in self.headers.get('Accept', '')
        assert 'text/event-stream' in self.headers.get('Accept', '')
        if method != 'initialize':
            assert self.headers.get('Mcp-Session-Id') == ('synthetic-session' if self.stateful else None)
            assert self.headers.get('MCP-Protocol-Version') == '2025-03-26'
        if 'id' not in request:
            self.send_response(202); self.end_headers(); return
        if method == 'initialize':
            result = {'protocolVersion':'2025-03-26','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
        elif method == 'tools/list':
            result = {'tools':[{'name':'echo','description':'Echo input','inputSchema':{'type':'object','properties':{'text':{'type':'string'}}}}]}
        elif method == 'tools/call':
            time.sleep(float(os.environ.get('OAP_VERIFY_MCP_DELAY', '0')))
            result = {'content':[{'type':'text','text':'echoed:' + request['params']['arguments']['text']}]}
        else:
            raise AssertionError(method)
        payload = json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}).encode()
        self.send_response(200)
        if method == 'initialize' and self.stateful: self.send_header('Mcp-Session-Id', 'synthetic-session')
        if method == 'tools/list':
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            self.wfile.write(b'data: {"jsonrpc":"2.0","id":"ping-1","method":"ping"}\n\n'); self.wfile.flush()
            assert self.ping_answered.wait(3), 'client did not respond to ping while stream was open'
            self.wfile.write(b'data: ' + payload + b'\n\n'); self.wfile.flush(); return
        else:
            self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload)
    def do_DELETE(self):
        assert self.headers.get('Mcp-Session-Id') == 'synthetic-session'
        self.methods.append('DELETE')
        self.send_response(204); self.end_headers()


class Unconfigured(Mcp):
    methods = []
    ping_answered = threading.Event()


def main():
    output = Path(sys.argv[1]).resolve(); output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-mcp-http-') as temporary:
        root = Path(temporary).resolve(); home = root / 'home'; data = home / '.oapx'; data.mkdir(parents=True)
        binary = root / 'oxap'; shutil.copy2(base.ROOT / 'zig/zig-out/bin/oxap', binary)
        provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), stdio.Provider)
        mcp = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Mcp)
        unconfigured = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Unconfigured)
        scheme = 'http'
        if os.environ.get('OAP_VERIFY_TLS_CERT'):
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(os.environ['OAP_VERIFY_TLS_CERT'], os.environ['OAP_VERIFY_TLS_KEY'])
            mcp.socket = context.wrap_socket(mcp.socket, server_side=True)
            scheme = 'https'
        threads = [threading.Thread(target=s.serve_forever, daemon=True) for s in (provider, mcp, unconfigured)]
        for thread in threads: thread.start()
        url = f'http://127.0.0.1:{provider.server_port}/v1'
        (data / 'providers.json').write_text(json.dumps({'providers':[{'id':'synthetic','api':'openai-completions','base_url':url,'auth':'none','models':['synthetic-model']}]}))
        (data / 'config.json').write_text(json.dumps({'model':'synthetic-model','provider':'synthetic','api':'openai-completions','workspace':str(root)}))
        (root / 'hub.json').write_text(json.dumps({'adapters':{'loop':{'type':'oapx'}},'tool_sources':{'demo':{'kind':'remote','protocol':'mcp','endpoint':f'{scheme}://127.0.0.1:{mcp.server_port}/mcp'}}}))
        environment = {'PATH':'/usr/bin:/bin','HOME':str(home),'TMPDIR':str(root),'NODE_ENV':'test','OAPX_KEYCHAIN_SERVICE':'oap-verify-'+str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec','-f',str(profile),str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'http', flags=('--no-save-oapx-sessions',))
            rejected = base.envelope('session.open.request', {'session_id':'wire-source','tool_sources':[{'id':'unconfigured','kind':'remote','endpoint':f'http://127.0.0.1:{unconfigured.server_port}/mcp'}]})
            try:
                hub.call('open', adapter='loop', request=rejected)
            except AssertionError as error:
                response = error.args[0]
                assert isinstance(response, dict) and response['error']['code'] == 'unsupported_feature' and response['error']['details'].get('source') == 'unconfigured', response
            else:
                raise AssertionError('unconfigured remote source was accepted')
            assert not Unconfigured.methods, Unconfigured.methods
            feature = hub.call('capabilities', adapter='loop')['payload']['features']['action.tool_sources.attach']
            assert 'remote' in feature['modes'] and 'remote' in feature['limits']['transports'], feature
            request = base.envelope('session.open.request', {'session_id':'http-proof','tool_sources':[{'id':'demo','kind':'remote'}], 'metadata':{'oapx':{'permission_mode':'bypass'}}})
            if os.environ.get('OAP_VERIFY_TLS_EXPECT_REFUSAL'):
                try:
                    hub.call('open', adapter='loop', request=request)
                except AssertionError as error:
                    response = error.args[0]
                    assert isinstance(response, dict) and response['error']['code'] == 'probe_failed', response
                    assert not Mcp.methods, Mcp.methods
                    print('PASS: untrusted HTTPS server refused')
                    return
                raise AssertionError('untrusted HTTPS server was accepted')
            result = hub.call('open', adapter='loop', request=request)
            assert 'error' not in result.get('payload', {}), result
            catalog = hub.call('tools', session_id='http-proof')
            assert any(t['name'] == 'mcp_demo_echo' and t['source'] == 'demo' for t in catalog['payload']['tools']), catalog
            request = base.envelope('session.message.submit.request', {'session_id':'http-proof','delivery':'auto','messages':[{'role':'user','content':'Call the echo tool with hello'}]})
            request['session_id'] = 'http-proof'
            hub.call('submit', session_id='http-proof', request=request)
            deadline = time.monotonic() + 50
            while time.monotonic() < deadline:
                if hub.call('state', session_id='http-proof')['payload'].get('status') == 'idle': break
                time.sleep(0.05)
            else: raise AssertionError('run did not return to idle')
            assert 'echoed:hello' in json.dumps(stdio.Provider.requests[-1]), stdio.Provider.requests
            hub.stop(); hub = None
            initialize = base.envelope('protocol.initialize.request', {'protocol_versions':['0.1'], 'profiles':['open-agent-protocol.agent-control-core'], 'participant':{'id':'user'}})
            direct = subprocess.run(command + ['serve', 'agent', '--backend', 'loop', '--config', str(root / 'hub.json'), '--stdio'], input=''.join(json.dumps(x) + '\n' for x in [initialize, rejected]), text=True, cwd=root, env=environment, capture_output=True, timeout=30)
            frames = [json.loads(line) for line in direct.stdout.splitlines()]
            assert direct.returncode == 0 and any(frame.get('payload', {}).get('error', {}).get('details', {}).get('source') == 'unconfigured' for frame in frames), (direct.returncode, frames, direct.stderr)
            assert not Unconfigured.methods, Unconfigured.methods
            assert Mcp.ping_answered.is_set()
            expected = ['initialize','notifications/initialized','tools/list','tools/call']
            if Mcp.stateful: expected.append('DELETE')
            assert Mcp.methods == expected, Mcp.methods
            print(f'PASS: {len(Mcp.methods)} {scheme.upper()} lifecycle messages; tool result reached model')
        finally:
            if hub: hub.stop()
            for server in (provider, mcp, unconfigured): server.shutdown(); server.server_close()
            for thread in threads: thread.join()

if __name__ == '__main__': main()
