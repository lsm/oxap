#!/usr/bin/env python3
"""Exercise a configured stdio MCP tool through a served oapx session."""
import http.server
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import threading
import uuid

from importlib.machinery import SourceFileLoader
base = SourceFileLoader('restart', str(Path(__file__).with_name('oapx-session-restart.py'))).load_module()

class Provider(base.Provider):
    requests = []
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.requests.append(body)
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        if len(self.requests) == 1:
            delta = {'role': 'assistant', 'tool_calls': [{'index': 0, 'id': 'call_1', 'type': 'function', 'function': {'name': 'mcp_demo_echo', 'arguments': '{"text":"hello"}'}}]}
            finish = 'tool_calls'
        else:
            delta, finish = {'role': 'assistant', 'content': 'finished'}, 'stop'
        for part, end in [(delta, None), ({}, finish)]:
            value = {'id': 'synthetic', 'object': 'chat.completion.chunk', 'model': 'synthetic-model', 'choices': [{'index': 0, 'delta': part, 'finish_reason': end}]}
            self.wfile.write(('data: ' + json.dumps(value) + '\n\n').encode())
        self.wfile.write(b'data: [DONE]\n\n')
        self.wfile.flush()

SERVER = '''import json,sys
for line in sys.stdin:
 r=json.loads(line); m=r.get("method"); i=r.get("id")
 if i is None: continue
 if m=="initialize": result={"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
 elif m=="tools/list": result={"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}
 elif m=="tools/call": result={"content":[{"type":"text","text":"echoed:"+r["params"]["arguments"]["text"]}]}
 else: raise ValueError(m)
 print(json.dumps({"jsonrpc":"2.0","id":i,"result":result}),flush=True)
'''

def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-mcp-') as temporary:
        root = Path(temporary).resolve()
        home = root / 'home'; data = home / '.oapx'; data.mkdir(parents=True)
        binary = root / 'oapx'; shutil.copy2(base.ROOT / 'zig/zig-out/bin/oxap', binary)
        (root / 'server.py').write_text(SERVER)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        url = f'http://127.0.0.1:{server.server_port}/v1'
        (data / 'providers.json').write_text(json.dumps({'providers':[{'id':'synthetic','api':'openai-completions','base_url':url,'auth':'none','models':['synthetic-model']}]}))
        (data / 'config.json').write_text(json.dumps({'model':'synthetic-model','provider':'synthetic','api':'openai-completions','workspace':str(root)}))
        (root / 'hub.json').write_text(json.dumps({'adapters':{'loop':{'type':'oapx'}},'tool_sources':{'demo':{'kind':'process','protocol':'mcp','command':'/usr/bin/python3','args':[str(root / 'server.py')]}}}))
        environment = {'PATH':'/usr/bin:/bin','HOME':str(home),'TMPDIR':str(root),'NODE_ENV':'test','OAPX_KEYCHAIN_SERVICE':'oap-verify-'+str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec','-f',str(profile),str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'mcp', flags=('--no-save-oapx-sessions',))
            request = base.envelope('session.open.request', {'session_id':'mcp-proof','tool_sources':[{'id':'demo','kind':'process'}], 'metadata':{'oapx':{'permission_mode':'bypass'}}})
            result = hub.call('open', adapter='loop', request=request)
            assert 'error' not in result.get('payload', {}), result
            base.submit(hub, 'mcp-proof', 'Call the echo tool with hello')
            transcript = hub.call('work.read', session_id='mcp-proof')
            (output / 'provider-requests.json').write_text(json.dumps(Provider.requests, indent=2))
            assert 'mcp_demo_echo' in json.dumps(Provider.requests[0].get('tools', [])), Provider.requests
            assert 'echoed:hello' in json.dumps(Provider.requests[1]), Provider.requests
            assert 'finished' in json.dumps(transcript), transcript
            print('PASS: discovered and called stdio MCP tool')
        finally:
            if hub: hub.stop()
            server.shutdown(); server.server_close(); thread.join()

if __name__ == '__main__': main()
