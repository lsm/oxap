#!/usr/bin/env python3
"""Exercise catalog headers with an isolated generated test catalog."""
import http.server
import importlib.util
import json
import os
import queue
import socket
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


class Provider(base.Provider):
    observed = []

    def record(self):
        self.observed.append((self.command,
                              self.headers.get('X-Example') == 'fixture-tenant-value',
                              self.headers.get('X-Literal') == 'literal',
                              self.headers.get('X-Override') == 'override',
                              self.headers.get_all('User-Agent') == ['synthetic-catalog/1']))

    def do_GET(self):
        self.record()
        super().do_GET()

    def do_POST(self):
        self.record()
        super().do_POST()


def missing_header_inference(binary, environment, root):
    clean = dict(environment)
    clean.pop('EXAMPLE_HEADER', None)
    process = subprocess.Popen([str(binary), 'serve', 'provider', '--stdio'], cwd=root,
                               env=clean, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True)
    frames = queue.Queue()
    def read():
        for line in process.stdout:
            frames.put(json.loads(line))
        frames.put(None)
    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    def send(kind, identifier, payload):
        frame = {'protocol': 'open-agent-protocol', 'version': '0.1',
                 'profile': 'open-agent-protocol.model-provider-core',
                 'type': kind, 'id': identifier, 'payload': payload}
        process.stdin.write(json.dumps(frame) + '\n')
        process.stdin.flush()
    def receive(kind):
        while True:
            frame = frames.get(timeout=30)
            assert frame is not None, f'endpoint exited before {kind}'
            if frame['type'] == kind:
                return frame
    try:
        send('provider.credential.grant.request', 'grant', {'provider_id': 'deepseek', 'nonce': 'fixture-nonce'})
        channel = receive('provider.credential.grant.channel')['payload']['channel']
        with socket.socket(socket.AF_UNIX) as client:
            client.connect(channel)
            client.sendall(b'fixture-nonce\nsynthetic-side-channel-value')
        granted = receive('provider.credential.grant.response')['payload']
        assert granted['accepted'], granted
        send('inference.create.request', 'infer', {'model_ref': 'deepseek/anthropic-messages@synthetic-model',
             'messages': [{'role': 'user', 'content': 'hello'}], 'credential_ref': granted['credential_ref']})
        response = receive('inference.create.response')
        assert response['payload']['accepted'], response
        failed = receive('inference.failed')
        assert failed['payload']['error']['code'] == 'invalid_request', failed
        send('provider.describe.request', 'after', {})
        assert receive('provider.describe.response')['in_reply_to'] == 'after'
    finally:
        if process.poll() is None:
            process.stdin.close()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
        reader.join(timeout=5)
        process.stdout.close()


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix='oap-catalog-headers-') as temporary:
        root = Path(temporary).resolve()
        source = root / 'source'
        shutil.copytree(base.ROOT, source, ignore=shutil.ignore_patterns('.git', '.zig-cache', 'zig-out', 'node_modules', 'target'))
        catalog_path = source / 'providers/catalog.json'
        catalog = json.loads(catalog_path.read_text())
        row = next(row for row in catalog['providers'] if row['id'] == 'deepseek')
        row['headers'] = {'X-Example': {'env': 'EXAMPLE_HEADER'}, 'X-Literal': 'literal', 'User-Agent': 'synthetic-catalog/1'}
        catalog_path.write_text(json.dumps(catalog))
        home = root / 'home'
        data = home / '.oapx'
        data.mkdir(parents=True)
        environment = {'PATH': os.environ['PATH'], 'HOME': str(home), 'TMPDIR': str(root), 'NODE_ENV': 'test'}
        build = subprocess.run(['zig', 'build', '--build-file', 'zig/build.zig', '-Doptimize=ReleaseSafe'], cwd=source, env=environment, capture_output=True, text=True, timeout=600)
        (output / 'build.log').write_text(build.stdout + build.stderr + f'\nexit={build.returncode}\n')
        assert build.returncode == 0, 'fixture build failed; see build.log'
        binary = source / 'zig/zig-out/bin/oxap'
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f'http://127.0.0.1:{server.server_port}/v1'
        config = {'overrides': [{'id': 'deepseek', 'base_url': url, 'models': ['synthetic-model'], 'headers': {'X-Override': 'override'}}]}
        (data / 'providers.json').write_text(json.dumps(config))
        (data / 'config.json').write_text(json.dumps({'model': 'synthetic-model', 'provider': 'deepseek', 'api': 'openai-completions', 'workspace': str(root)}))
        (root / 'hub.json').write_text(json.dumps({'adapters': {'loop': {'type': 'oapx'}}}))
        environment.update({'EXAMPLE_HEADER': 'fixture-tenant-value', 'OAPX_KEYCHAIN_SERVICE': 'oap-verify-' + str(uuid.uuid4())})
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny network*)\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec', '-f', str(profile), str(binary)]
        hub = None
        try:
            hub = base.Hub(command, environment, root, output, 'headers', flags=('--no-save-oapx-sessions',))
            base.open_session(hub, 'headers-proof')
            base.submit(hub, 'headers-proof', 'synthetic header turn')
            transcript = hub.call('work.read', session_id='headers-proof')
            assert base.REPLY in json.dumps(transcript), transcript
            assert any(row[0] == 'GET' for row in Provider.observed), Provider.observed
            assert any(row[0] == 'POST' for row in Provider.observed), Provider.observed
            assert all(all(row[1:]) for row in Provider.observed), Provider.observed
            hub.stop()
            hub = None
            for artifact in output.iterdir():
                assert 'fixture-tenant-value' not in artifact.read_text(), artifact.name
            count = len(Provider.observed)
            missing_header_inference(binary, environment, root)
            del environment['EXAMPLE_HEADER']
            config['providers'] = [{'id': 'healthy', 'api': 'openai-completions', 'base_url': url, 'auth': 'none', 'models': ['synthetic-model']}]
            (data / 'providers.json').write_text(json.dumps(config))
            (data / 'config.json').write_text(json.dumps({'model': 'synthetic-model', 'provider': 'healthy', 'api': 'openai-completions', 'workspace': str(root)}))
            hub = base.Hub(command, environment, root, output, 'missing', flags=('--no-save-oapx-sessions',))
            base.open_session(hub, 'missing-header')
            base.submit(hub, 'missing-header', 'healthy provider turn')
            transcript = hub.call('work.read', session_id='missing-header')
            assert base.REPLY in json.dumps(transcript), transcript
            hub.stop()
            hub = None
            assert len(Provider.observed) > count, 'healthy provider sent no requests'
            print(f'PASS: {count} requests carried catalog and override headers; healthy provider remained usable')
        finally:
            if hub:
                hub.stop()
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    main()
