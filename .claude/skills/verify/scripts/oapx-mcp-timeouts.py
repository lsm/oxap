#!/usr/bin/env python3
"""Check that timed-out MCP discovery does not leave HTTP workers behind."""
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import uuid

spec = importlib.util.spec_from_file_location('restart', Path(__file__).with_name('oapx-session-restart.py'))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)

class Hanging(http.server.BaseHTTPRequestHandler):
    release = threading.Event()
    requests = 0
    def log_message(self, *args):
        pass
    def do_POST(self):
        self.rfile.read(int(self.headers['Content-Length']))
        type(self).requests += 1
        self.release.wait(40)


def threads(pid):
    if sys.platform == 'darwin':
        return len(subprocess.check_output(['ps', '-M', '-p', str(pid)], text=True).splitlines()) - 1
    return len(list((Path('/proc') / str(pid) / 'task').iterdir()))


def main():
    output = Path(sys.argv[1]).resolve(); output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-mcp-timeout-') as temporary:
        root = Path(temporary).resolve(); home = root / 'home'; data = home / '.oapx'; data.mkdir(parents=True)
        binary = root / 'oxap'; shutil.copy2(base.ROOT / 'zig/zig-out/bin/oxap', binary)
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Hanging)
        worker = threading.Thread(target=server.serve_forever, daemon=True); worker.start()
        (data / 'providers.json').write_text(json.dumps({'providers':[{'id':'synthetic','api':'openai-completions','base_url':'http://127.0.0.1:9/v1','auth':'none','models':['synthetic-model']}]}))
        (data / 'config.json').write_text(json.dumps({'model':'synthetic-model','provider':'synthetic','api':'openai-completions','workspace':str(root)}))
        (root / 'hub.json').write_text(json.dumps({'adapters':{'loop':{'type':'oapx'}},'tool_sources':{'hang':{'kind':'remote','protocol':'mcp','endpoint':f'http://127.0.0.1:{server.server_port}/mcp'}}}))
        environment = {'PATH':'/usr/bin:/bin','HOME':str(home),'TMPDIR':str(root),'NODE_ENV':'test','OAPX_KEYCHAIN_SERVICE':'oap-verify-'+str(uuid.uuid4())}
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec','-f',str(profile),str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'timeouts', flags=('--no-save-oapx-sessions',))
            before = threads(hub.process.pid)
            for number in range(4):
                request = base.envelope('session.open.request', {'session_id':f'timeout-{number}','tool_sources':[{'id':'hang','kind':'remote'}]})
                try:
                    hub.call('open', adapter='loop', request=request)
                except AssertionError as error:
                    response = error.args[0]
                    assert isinstance(response, dict) and response['error']['code'] == 'probe_failed', response
                else:
                    raise AssertionError('hanging server was accepted')
            after = threads(hub.process.pid)
            assert Hanging.requests == 4, Hanging.requests
            assert after <= before + 1, f'threads before={before}, after={after}'
            print(f'PASS: 4 timeouts; threads before={before}, after={after}')
        finally:
            Hanging.release.set()
            if hub: hub.stop()
            server.shutdown(); server.server_close(); worker.join()

if __name__ == '__main__': main()
