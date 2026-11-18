#!/usr/bin/env python3
"""Exercise a configured stdio MCP tool through a served oapx session."""
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import uuid

import importlib.util
spec = importlib.util.spec_from_file_location('restart', Path(__file__).with_name('oapx-session-restart.py'))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)

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

SERVER = '''import json,os,signal,sys
signal.signal(signal.SIGTERM, signal.SIG_IGN)
assert os.environ.get("EXAMPLE_ALLOWED") == "fixture"
assert "EXAMPLE_PRIVATE" not in os.environ
open(os.environ["EXAMPLE_START"], "a").write("start")
for line in sys.stdin:
 r=json.loads(line); m=r.get("method"); i=r.get("id")
 if i is None: continue
 if m=="initialize": result={"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
 elif m=="tools/list": result={"tools":[{"name":"echo","description":"Echo input","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}
 elif m=="tools/call": result={"content":[{"type":"text","text":"echoed:"+r["params"]["arguments"]["text"]}]}
 else: raise ValueError(m)
 print(json.dumps({"jsonrpc":"2.0","id":i,"result":result}),flush=True)
open(os.environ["EXAMPLE_MARKER"], "w").write("closed")
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
        (root / 'hub.json').write_text(json.dumps({'adapters':{'loop':{'type':'oapx'}},'tool_sources':{'demo':{'kind':'process','protocol':'mcp','display_name':'Synthetic MCP service','command':'/usr/bin/python3','args':[str(root / 'server.py')], 'environment':['EXAMPLE_ALLOWED=fixture', 'EXAMPLE_MARKER=' + str(root / 'closed'), 'EXAMPLE_START=' + str(root / 'started')]}}}))
        environment = {'PATH':'/usr/bin:/bin','HOME':str(home),'TMPDIR':str(root),'NODE_ENV':'test','EXAMPLE_PRIVATE':'must-not-inherit','OAPX_KEYCHAIN_SERVICE':'oap-verify-'+str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec','-f',str(profile),str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'mcp', flags=('--no-save-oapx-sessions',))
            bad = base.envelope('session.open.request', {'session_id':'bad-source','tool_sources':[{'id':'remote-example','kind':'remote'}]})
            try:
                hub.call('open', adapter='loop', request=bad)
                raise AssertionError('remote source was accepted')
            except AssertionError as error:
                response = error.args[0]
                assert isinstance(response, dict) and response['error']['details'].get('source') == 'remote-example', response
            invalid = base.envelope('session.open.request', {'session_id':'invalid-model','tool_sources':[{'id':'demo','kind':'process'}], 'metadata':{'oapx':{'model':'unknown-model'}}})
            try:
                hub.call('open', adapter='loop', request=invalid)
                raise AssertionError('unknown model was accepted')
            except AssertionError as error:
                assert isinstance(error.args[0], dict), error
            assert not (root / 'started').exists(), 'invalid open started the MCP server'
            request = base.envelope('session.open.request', {'session_id':'mcp-proof','tool_sources':[{'id':'demo','kind':'process'}], 'tools':[{'name':'host_lookup','execution_owner':'user','source':'demo','input_schema':{'type':'object'}}], 'metadata':{'oapx':{'permission_mode':'bypass'}}})
            result = hub.call('open', adapter='loop', request=request)
            assert 'error' not in result.get('payload', {}), result
            assert any(source['id'] == 'demo' for source in result['payload']['sources']), result
            state = hub.call('state', session_id='mcp-proof')
            assert any(source['id'] == 'demo' and source['display_name'] == 'Synthetic MCP service' and source['protocol'] == 'mcp' for source in state['payload']['sources']), state
            catalog = hub.call('tools', session_id='mcp-proof')
            assert any(tool['name'] == 'mcp_demo_echo' and tool['source'] == 'demo' for tool in catalog['payload']['tools']), catalog
            assert any(tool['name'] == 'host_lookup' and tool['source'] == 'demo' for tool in catalog['payload']['tools']), catalog
            collision = base.envelope('session.open.request', {'session_id':'collision', 'tool_sources':[{'id':'demo','kind':'process'}], 'tools':[{'name':'mcp_demo_echo','execution_owner':'user','source':'demo','input_schema':{'type':'object'}}]})
            try:
                hub.call('open', adapter='loop', request=collision)
                raise AssertionError('colliding tool was accepted')
            except AssertionError as error:
                response = error.args[0]
                details = response['error']['details'] if isinstance(response, dict) else {}
                assert details.get('feature') == 'action.tools.provide' and details.get('tool') == 'mcp_demo_echo', response
            assert (root / 'closed').read_text() == 'closed'
            (root / 'closed').unlink()
            hub.call('events', session_id='mcp-proof')
            base.submit(hub, 'mcp-proof', 'Call the echo tool with hello')
            events = [json.loads(line) for line in (output / 'mcp.jsonl').read_text().splitlines()]
            calls = [event['envelope'] for event in events if event.get('event') == 'envelope' and event['envelope']['type'] == 'action.call.started']
            assert any(call['payload'].get('source') == 'demo' for call in calls), calls
            transcript = hub.call('work.read', session_id='mcp-proof')
            (output / 'provider-requests.json').write_text(json.dumps(Provider.requests, indent=2))
            assert 'mcp_demo_echo' in json.dumps(Provider.requests[0].get('tools', [])), Provider.requests
            assert 'echoed:hello' in json.dumps(Provider.requests[1]), Provider.requests
            assert 'finished' in json.dumps(transcript), transcript
            hub.stop(); hub = None
            assert (root / 'closed').read_text() == 'closed', 'server did not exit on stdin EOF'
            initialize = base.envelope('protocol.initialize.request', {'protocol_versions':['0.1'], 'profiles':['open-agent-protocol.agent-control-core'], 'participant':{'id':'user'}})
            opens = [base.envelope('session.open.request', {'session_id':'duplicate-proof'}),
                     base.envelope('session.open.request', {'session_id':'duplicate-proof', 'tool_sources':[{'id':'remote-example','kind':'remote'}]}),
                     base.envelope('session.open.request', {'session_id':'duplicate-proof', 'tool_sources':[{'id':'blocked','kind':'process','command':'/bin/true'}]})]
            direct = subprocess.run(command + ['serve', 'agent', '--backend', 'oapx', '--stdio'], input=''.join(json.dumps(x) + '\n' for x in [initialize, *opens]), text=True, cwd=root, env=environment, capture_output=True, timeout=30)
            (output / 'direct.jsonl').write_text(direct.stdout)
            frames = [json.loads(line) for line in direct.stdout.splitlines()]
            assert direct.returncode == 0 and any(x.get('payload', {}).get('error', {}).get('code') == 'session_exists' for x in frames), (direct.returncode, frames, direct.stderr)
            assert any(x.get('payload', {}).get('error', {}).get('details', {}).get('source') == 'blocked' and x['payload']['error']['code'] == 'unsupported_feature' for x in frames), frames
            print('PASS: discovered and called stdio MCP tool')
        finally:
            if hub: hub.stop()
            server.shutdown(); server.server_close(); thread.join()

if __name__ == '__main__': main()
