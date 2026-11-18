#!/usr/bin/env python3
"""Run a built-in loop turn inside the Alpine image."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import threading
import http.server

spec = importlib.util.spec_from_file_location('restart', Path(__file__).with_name('oapx-session-restart.py'))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)

output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=False)
with tempfile.TemporaryDirectory(prefix='oxap-alpine-') as temporary:
    root = Path(temporary)
    data = root / 'home' / '.oapx'
    data.mkdir(parents=True)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), base.Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    (data / 'providers.json').write_text(json.dumps({'providers': [{'id': 'synthetic',
        'api': 'openai-completions', 'base_url': f'http://127.0.0.1:{server.server_port}/v1',
        'auth': 'none', 'models': ['synthetic-model']}]}))
    (data / 'config.json').write_text(json.dumps({'model': 'synthetic-model',
        'provider': 'synthetic', 'api': 'openai-completions', 'workspace': str(root)}))
    (root / 'hub.json').write_text(json.dumps({'adapters': {'loop': {'type': 'oapx'}}}))
    command = ['docker', 'run', '--rm', '-i', '--network', 'host', '-v', f'{root}:{root}',
               '-w', str(root), '-e', f'HOME={root}/home', '-e', 'NODE_ENV=test', 'oxap-alpine:verify']
    hub = None
    try:
        hub = base.Hub(command, {'PATH': '/usr/bin:/bin'}, root, output, 'alpine',
                       flags=('--no-save-oapx-sessions',))
        base.open_session(hub, 'alpine-proof')
        base.submit(hub, 'alpine-proof', 'synthetic Alpine turn')
        transcript = hub.call('work.read', session_id='alpine-proof')
        assert base.REPLY in json.dumps(transcript), transcript
        assert len(base.Provider.requests) == 1, base.Provider.requests
        print('PASS: Alpine image completed one fake-endpoint turn; requests=1')
    finally:
        if hub:
            hub.stop()
        server.shutdown()
        server.server_close()
        thread.join()
