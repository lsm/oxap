#!/usr/bin/env python3
"""Exercise environment-backed custom provider headers through a served turn."""
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
        self.observed.append((self.command, self.headers.get('X-Example') == 'fixture-tenant-value', self.headers.get('X-Literal') == 'literal'))
    def do_GET(self):
        self.record()
        super().do_GET()
    def do_POST(self):
        self.record()
        super().do_POST()


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-provider-headers-') as temporary:
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
        config = {'providers':[{'id':'synthetic','api':'openai-completions','base_url':url,'auth':'none','models':['synthetic-model'],'headers':{'X-Example':{'env':'EXAMPLE_HEADER'},'X-Literal':'literal'}}]}
        (data / 'providers.json').write_text(json.dumps(config))
        (data / 'config.json').write_text(json.dumps({'model':'synthetic-model','provider':'synthetic','api':'openai-completions','workspace':str(root)}))
        (root / 'hub.json').write_text(json.dumps({'adapters':{'loop':{'type':'oapx'}}}))
        environment = {'PATH':'/usr/bin:/bin','HOME':str(home),'TMPDIR':str(root),'NODE_ENV':'test','EXAMPLE_HEADER':'fixture-tenant-value','OAPX_KEYCHAIN_SERVICE':'oap-verify-'+str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec','-f',str(profile),str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'headers', flags=('--no-save-oapx-sessions',))
            base.open_session(hub, 'headers-proof')
            base.submit(hub, 'headers-proof', 'synthetic header turn')
            transcript = hub.call('work.read', session_id='headers-proof')
            assert base.REPLY in json.dumps(transcript), transcript
            assert Provider.observed and any(row[0] == 'POST' for row in Provider.observed) and all(row[1] and row[2] for row in Provider.observed), Provider.observed
            hub.stop(); hub = None
            for artifact in output.iterdir():
                assert 'fixture-tenant-value' not in artifact.read_text(), artifact.name
            assert 'fixture-tenant-value' not in (data / 'providers.json').read_text()
            count = len(Provider.observed)
            config['providers'][0]['headers']['X-Example']['env'] = 'OXAP_TEST_HEADER_ABSENT_1849'
            (data / 'providers.json').write_text(json.dumps(config))
            hub = base.Hub(command, environment, root, output, 'missing', flags=('--no-save-oapx-sessions',))
            try:
                base.open_session(hub, 'missing-header')
                base.submit(hub, 'missing-header', 'synthetic missing header turn')
            except AssertionError as error:
                response = error.args[0]
                assert isinstance(response, dict) and response.get('error', {}).get('code') == 'internal', response
            else:
                raise AssertionError('missing environment was accepted')
            hub.stop(); hub = None
            assert len(Provider.observed) == count, 'missing environment sent a provider request'
            print(f'PASS: {count} provider requests carried both headers; missing environment sent none')
        finally:
            if hub: hub.stop()
            server.shutdown(); server.server_close(); thread.join()

if __name__ == '__main__': main()
