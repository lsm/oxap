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
    def do_POST(self):
        self.observed.append((self.headers.get('X-Example') == 'fixture-tenant-value', self.headers.get('X-Literal') == 'literal'))
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
            assert Provider.observed and all(all(pair) for pair in Provider.observed), Provider.observed
            hub.stop(); hub = None
            for artifact in output.iterdir():
                assert 'fixture-tenant-value' not in artifact.read_text(), artifact.name
            assert 'fixture-tenant-value' not in (data / 'providers.json').read_text()
            print(f'PASS: {len(Provider.observed)} provider requests carried both headers')
        finally:
            if hub: hub.stop()
            server.shutdown(); server.server_close(); thread.join()

if __name__ == '__main__': main()
