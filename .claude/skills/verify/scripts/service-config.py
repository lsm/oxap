import argparse
import http.client
import json
import os
from pathlib import Path
import runpy
import select
import signal
import subprocess
import sys
import tempfile


def cases():
    fixtures = Path(__file__).resolve().parents[4] / 'go/validation/testdata/service-invalid.json'
    for case in json.loads(fixtures.read_text()):
        yield json.dumps(case['config']), case['diagnostic']
    yield '{}', 'at least one service'
    yield 'null', 'must be an object'
    yield '{"agent":{},"agent":{}}', 'line 1, column 13: duplicate mapping key'
    yield 'provider:\n  stdio: raw\n  "stdio": raw', 'line 3, column 3: duplicate mapping key'
    yield 'provider: {stdio: raw, stdio: raw}', 'duplicate mapping key'
    yield 'provider: {stdio: raw} # provider\nextra: {}', 'unknown field "extra"'
    yield 'hub: null', 'hub is reserved'
    yield 'agent: null', 'agent: must be an object'
    yield 'agent: {}', 'exactly one of http or stdio'
    yield 'provider: {}', 'exactly one of http or stdio'
    yield 'agent: {http: localhost:0, stdio: multi_session}', 'exactly one of http or stdio'
    yield 'provider: {stdio: multi_session}', 'invalid provider stdio binding'
    yield 'agent: {stdio: typo}', 'invalid agent stdio binding'
    yield 'agent: {stdio: multi_session, adapter: one}', 'only valid for stdio: raw'
    yield 'agent: {http: null}', 'explicit address'
    yield 'agent: {http: localhost}', 'invalid HTTP address'
    yield 'agent: {http: 0.0.0.0:8000}', 'loopback'
    yield 'provider: {http: 127.0.0.1:0}', 'nonzero port'
    yield 'agent: {stdio: raw}', 'adapter name is required'
    yield 'agent: {stdio: raw, adapter: missing}', 'no adapter named "missing"'
    yield 'agent: {stdio: multi_session, adapters: {a: {type: unknown}}}', 'does not know'
    yield 'agent: {stdio: multi_session}\nprovider: {stdio: raw}', 'exclusive use of stdio'
    yield 'provider: {http: 127.0.0.1:8000}\nagent: {http: localhost:08000}', 'HTTP listeners conflict'
    yield 'provider: &p {stdio: raw}', 'unsupported YAML indicator'
    yield 'provider: *p', 'unsupported YAML indicator'
    yield 'provider: !custom {stdio: raw}', 'unsupported YAML indicator'
    yield 'provider: {<<: {stdio: raw}}', 'merge keys are unsupported'
    yield 'provider: |\n  stdio: raw', 'unsupported YAML indicator'
    yield 'provider: {stdio: raw}\n---\nprovider: {stdio: raw}', 'another document'
    yield 'provider:\n\tstdio: raw', 'tabs cannot indent a block'
    yield 'provider: {stdio: "raw}', 'unterminated quoted string'
    yield 'provider: {stdio: raw extra}', 'invalid provider stdio binding'
    yield 'provider: {true: raw}', 'mapping keys must be strings'
    yield 'agent: {stdio: multi_session, adapters: {memory: {any_directory: yes}}}', 'must be a boolean'
    yield 'agent: {stdio: multi_session, tool_sources: {t: {kind: remote, auth: null}}}', 'auth must name an environment variable'
    yield 'agent: {stdio: multi_session, tool_sources: {t: {kind: remote, auth: {env: UNSET_TOOL_TOKEN}}}}', 'environment variable "UNSET_TOOL_TOKEN" is not set'
    yield 'agent: {stdio: multi_session, adapters: {memory: {agent_config: ' + '[' * 70 + '0' + ']' * 70 + '}}}', 'nesting limit exceeded'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=lambda path: str(Path(path).resolve()))
    args = parser.parse_args()
    helpers = runpy.run_path(str(Path(__file__).with_name('hub-open-directory.py')))
    with tempfile.TemporaryDirectory(prefix='service-config-') as temp:
        root = Path(temp)
        home = root / 'home'
        home.mkdir()
        config = root / 'server.yaml'
        env = {'HOME': str(home), 'PATH': os.defpath, 'NODE_ENV': 'test',
               'XDG_DATA_HOME': str(root / 'data'), 'XDG_CONFIG_HOME': str(root / 'config')}

        def run(document, requests=()):
            config.write_text(document if isinstance(document, str) else json.dumps(document))
            return subprocess.run([args.binary, 'serve', '--config', config], cwd=root, env=env, capture_output=True, text=True,
                                  input=''.join(json.dumps(line) + '\n' for line in requests), timeout=15)

        invalid_count = 0
        for document, diagnostic in cases():
            result = run(document)
            assert result.returncode == 1 and not result.stdout and diagnostic in result.stderr, (document, result)
            assert not list(home.iterdir()) and not (root / 'data').exists() and not (root / 'config').exists()
            invalid_count += 1
            print(f'refused with exit {result.returncode}: {result.stderr.strip()}')

        peer = root / 'peer.py'
        peer.write_text(helpers['PEER'])
        native = root / 'native.jsonl'
        adapters = {
            'scripted': {'type': 'memory'},
            'synthetic': {'type': 'codex', 'executable': sys.executable, 'args': [str(peer), str(native)],
                          'working_directory': str(root), 'model': 'synthetic-model',
                          'approval_policy': 'on-request', 'sandbox': 'workspace-write'},
        }
        envelope = helpers['envelope']
        capabilities = {**envelope('caps'), 'type': 'capabilities.request', 'payload': {}}
        for name, endpoint_id in [('scripted', 'reference.memory'), ('synthetic', 'codex.app-server')]:
            agent = {'adapters': adapters, 'stdio': 'raw', 'adapter': name}
            result = run({'agent': agent}, [capabilities, envelope(name)])
            assert result.returncode == 0, result
            frames = [json.loads(line) for line in result.stdout.splitlines()]
            endpoints = [frame['payload']['endpoint']['id'] for frame in frames
                         if frame['type'] == 'capabilities.response']
            assert endpoints == [endpoint_id], frames
            opened = [frame['payload']['session_id'] for frame in frames if frame['type'] == 'session.open.response']
            assert opened == [name], frames
            if name == 'scripted':
                assert not native.exists()
            else:
                assert 'thread/start' in native.read_text()
                native.unlink()
            print(f'raw stdio used {endpoint_id} and exited {result.returncode}')

        script = json.dumps(str(peer))
        log = json.dumps(str(native))
        executable = json.dumps(sys.executable)
        work = json.dumps(str(root))
        yaml_adapters = f'''adapters:
    scripted:
      type: memory
      agent_config:
        options:
          - label: 'it''s # literal'
            on: off
          - {{ enabled: True, value: ~ }}
    synthetic:
      type: codex # start the synthetic peer
      executable: {executable}
      args:
        - {script}
        - {log}
      working_directory: {work}
      model: "synthetic-model"
      approval_policy: on-request
      sandbox: workspace-write
'''
        requests = [{'id': index, 'op': 'open', 'adapter': name, 'request': envelope(name)}
                    for index, name in enumerate(adapters, 1)]
        result = run('---\nagent:\n  stdio: multi_session\n  ' + yaml_adapters + '...\n', requests)
        assert result.returncode == 0, result
        replies = {frame['id']: frame for line in result.stdout.splitlines()
                   if 'id' in (frame := json.loads(line))}
        for request in requests:
            helpers['opened'](replies[request['id']], request['adapter'])
        methods = [json.loads(line)['request']['method'] for line in native.read_text().splitlines()]
        assert methods.count('thread/start') == 1, result
        print(f'multi-session opened {len(requests)} sessions and exited {result.returncode}')

        config.write_text('agent:\n  http: localhost:0\n  ' + yaml_adapters)
        with subprocess.Popen([args.binary, 'serve', '--config', config], cwd=root, env=env, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, text=True) as process:
            try:
                assert select.select([process.stdout], [], [], 15)[0], 'HTTP listener did not start'
                host, port = process.stdout.readline().strip().removeprefix('listening on http://').split(':')
                assert host == '127.0.0.1' and int(port) > 0, (host, port)
                connection = http.client.HTTPConnection(host, int(port), timeout=5)
                for header, expected in [(host, 200), ('unexpected.invalid', 403)]:
                    try:
                        connection.request('GET', '/adapters', headers={'Host': header})
                        response = connection.getresponse()
                        body = response.read().decode()
                        assert response.status == expected, (response.status, body)
                        if expected == 200:
                            assert {entry['name'] for entry in json.loads(body)['adapters']} == set(adapters), body
                    finally:
                        connection.close()
                process.send_signal(signal.SIGTERM)
                _, stderr = process.communicate(timeout=10)
                assert process.returncode == 0, (process.returncode, stderr)
                print(f'multi-session HTTP listed {len(adapters)} adapters on loopback, rejected a foreign Host, and exited {process.returncode}')
            finally:
                if process.poll() is None:
                    process.kill()

        print(f'PASS: {invalid_count} refusals, 2 raw sessions, {len(requests)} multi-session opens, 2 HTTP responses')


if __name__ == '__main__':
    main()
