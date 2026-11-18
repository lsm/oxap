#!/usr/bin/env python3
"""Observe a served oapx session call a synthetic Streamable HTTP MCP tool."""
import http.server
import importlib.util
import json
import os
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

class Mcp(http.server.BaseHTTPRequestHandler):
    methods = []
    def log_message(self, *args):
        pass
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method = request['method']
        self.methods.append(method)
        assert 'application/json' in self.headers.get('Accept', '')
        assert 'text/event-stream' in self.headers.get('Accept', '')
        if method != 'initialize':
            assert self.headers.get('Mcp-Session-Id') == 'synthetic-session'
        if 'id' not in request:
            self.send_response(202); self.end_headers(); return
        if method == 'initialize':
            result = {'protocolVersion':'2025-03-26','capabilities':{'tools':{}},'serverInfo':{'name':'fixture','version':'1'}}
        elif method == 'tools/list':
            result = {'tools':[{'name':'echo','description':'Echo input','inputSchema':{'type':'object','properties':{'text':{'type':'string'}}}}]}
        elif method == 'tools/call':
            result = {'content':[{'type':'text','text':'echoed:' + request['params']['arguments']['text']}]}
        else:
            raise AssertionError(method)
        payload = json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}).encode()
        self.send_response(200)
        if method == 'initialize': self.send_header('Mcp-Session-Id', 'synthetic-session')
        if method == 'tools/list':
            payload = b'data: ' + payload + b'\n\n'
            self.send_header('Content-Type', 'text/event-stream')
        else:
            self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload)
    def do_DELETE(self):
        assert self.headers.get('Mcp-Session-Id') == 'synthetic-session'
        self.methods.append('DELETE')
        self.send_response(204); self.end_headers()


def main():
    output = Path(sys.argv[1]).resolve(); output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-mcp-http-') as temporary:
        root = Path(temporary).resolve(); home = root / 'home'; data = home / '.oapx'; data.mkdir(parents=True)
        binary = root / 'oxap'; shutil.copy2(base.ROOT / 'zig/zig-out/bin/oxap', binary)
        provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), stdio.Provider)
        mcp = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Mcp)
        threads = [threading.Thread(target=s.serve_forever, daemon=True) for s in (provider, mcp)]
        for thread in threads: thread.start()
        url = f'http://127.0.0.1:{provider.server_port}/v1'
        (data / 'providers.json').write_text(json.dumps({'providers':[{'id':'synthetic','api':'openai-completions','base_url':url,'auth':'none','models':['synthetic-model']}]}))
        (data / 'config.json').write_text(json.dumps({'model':'synthetic-model','provider':'synthetic','api':'openai-completions','workspace':str(root)}))
        (root / 'hub.json').write_text(json.dumps({'adapters':{'loop':{'type':'oapx'}},'tool_sources':{'demo':{'kind':'remote','protocol':'mcp','endpoint':f'http://127.0.0.1:{mcp.server_port}/mcp'}}}))
        environment = {'PATH':'/usr/bin:/bin','HOME':str(home),'TMPDIR':str(root),'NODE_ENV':'test','OAPX_KEYCHAIN_SERVICE':'oap-verify-'+str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec','-f',str(profile),str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'http', flags=('--no-save-oapx-sessions',))
            request = base.envelope('session.open.request', {'session_id':'http-proof','tool_sources':[{'id':'demo','kind':'remote'}], 'metadata':{'oapx':{'permission_mode':'bypass'}}})
            result = hub.call('open', adapter='loop', request=request)
            assert 'error' not in result.get('payload', {}), result
            catalog = hub.call('tools', session_id='http-proof')
            assert any(t['name'] == 'mcp_demo_echo' and t['source'] == 'demo' for t in catalog['payload']['tools']), catalog
            base.submit(hub, 'http-proof', 'Call the echo tool with hello')
            assert 'echoed:hello' in json.dumps(stdio.Provider.requests[-1]), stdio.Provider.requests
            hub.stop(); hub = None
            assert Mcp.methods == ['initialize','notifications/initialized','tools/list','tools/call','DELETE'], Mcp.methods
            print(f'PASS: {len(Mcp.methods)} HTTP lifecycle messages; tool result reached model')
        finally:
            if hub: hub.stop()
            for server in (provider, mcp): server.shutdown(); server.server_close()
            for thread in threads: thread.join()

if __name__ == '__main__': main()
