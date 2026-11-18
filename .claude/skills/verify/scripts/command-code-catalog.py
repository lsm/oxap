#!/usr/bin/env python3
"""Exercise Command Code discovery and inference against a loopback provider."""
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

spec = importlib.util.spec_from_file_location('restart', Path(__file__).with_name('oapx-session-restart.py'))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)


class Provider(base.Provider):
    observed = []

    def record(self):
        self.observed.append((self.command, self.path,
                              self.headers.get('x-cmd-zdr') == '1',
                              self.headers.get_all('User-Agent') == ['oxap/1']))

    def do_GET(self):
        self.record()
        data = {'data': [{'id': 'synthetic-model', 'supported_endpoints': ['/chat/completions']},
                         {'id': 'messages-only', 'supported_endpoints': ['/messages']},
                         {'id': 'unknown-endpoint'}]}
        self.send_response(200)
        self.end_headers()
        self.wfile.write(json.dumps(data).encode())

    def do_POST(self):
        self.record()
        super().do_POST()


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-command-code-') as temporary:
        root = Path(temporary).resolve()
        home = root / 'home'
        data = home / '.oapx'
        data.mkdir(parents=True)
        binary = root / 'oxap'
        shutil.copy2(base.ROOT / 'zig/zig-out/bin/oxap', binary)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f'http://127.0.0.1:{server.server_port}/v1'
        (data / 'providers.json').write_text(json.dumps({'overrides': [{'id': 'command-code', 'base_url': url}]}))
        (data / 'config.json').write_text(json.dumps({'model': 'synthetic-model', 'provider': 'command-code', 'api': 'openai-completions', 'workspace': str(root)}))
        (root / 'hub.json').write_text(json.dumps({'adapters': {'loop': {'type': 'oapx'}}}))
        environment = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'TMPDIR': str(root), 'NODE_ENV': 'test', 'OAPX_KEYCHAIN_SERVICE': 'oap-verify-' + str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec', '-f', str(profile), str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'command-code', flags=('--no-save-oapx-sessions',))
            base.open_session(hub, 'command-code-proof')
            listed = json.dumps(hub.call('models', session_id='command-code-proof', allow_degraded_features=['models.list']))
            assert 'synthetic-model' in listed and 'messages-only' not in listed and 'unknown-endpoint' not in listed, listed
            base.submit(hub, 'command-code-proof', 'synthetic command code turn')
            transcript = hub.call('work.read', session_id='command-code-proof')
            assert base.REPLY in json.dumps(transcript), transcript
            assert any(row[0] == 'GET' for row in Provider.observed), Provider.observed
            assert any(row[0] == 'POST' and row[1].endswith('/chat/completions') for row in Provider.observed), Provider.observed
            assert all(row[2] and row[3] for row in Provider.observed), Provider.observed
            print(f'PASS: chat model listed and answered; unsupported models omitted; {len(Provider.observed)} requests carried both headers')
        finally:
            if hub:
                hub.stop()
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    main()
