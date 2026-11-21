import argparse
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile


def cases():
    yield '{}', 'at least one service'
    yield 'null', 'must be an object'
    yield '{"agent":{},"agent":{}}', 'DuplicateField'
    yield '{"provider":{"binding":{"type":"raw_stdio","type":"http"}}}', 'DuplicateField'
    yield '{"provider":{"binding":{"type":"raw_stdio"}},"extra":{}}', 'unknown field "extra"'
    yield '{"hub":null}', 'hub is reserved'
    yield '{"agent":null}', 'agent: must be an object'
    yield '{"agent":{}}', 'agent.binding is required'
    yield '{"provider":{}}', 'provider.binding is required'
    yield '{"provider":{"binding":{"type":"multi_session_stdio"}}}', 'invalid provider binding'
    yield '{"agent":{"binding":{"type":"typo"}}}', 'invalid agent binding'
    yield '{"agent":{"binding":{"type":"multi_session_stdio","adapter":"one"}}}', 'only valid for raw_stdio'
    yield '{"agent":{"binding":{"type":"multi_session_stdio","address":"127.0.0.1:80"}}}', 'only valid for http'
    yield '{"agent":{"binding":{"type":"multi_session_stdio","extra":true}}}', 'unknown field "extra"'
    yield '{"agent":{"binding":{"type":"http"}}}', 'explicit address'
    yield '{"agent":{"binding":{"type":"http","address":"localhost"}}}', 'invalid HTTP address'
    yield '{"agent":{"binding":{"type":"http","address":"0.0.0.0:8000"}}}', 'loopback'
    yield '{"provider":{"binding":{"type":"http","address":"127.0.0.1:0"}}}', 'nonzero port'
    yield '{"agent":{"binding":{"type":"raw_stdio"}}}', 'adapter name is required'
    yield '{"agent":{"binding":{"type":"raw_stdio","adapter":"missing"}}}', 'no adapter named "missing"'
    yield '{"agent":{"binding":{"type":"multi_session_stdio"},"adapters":{"a":{"type":"memory","extra":0}}}}', 'unknown field "extra"'
    yield '{"agent":{"binding":{"type":"multi_session_stdio"},"adapters":{"a":{"type":"unknown"}}}}', 'does not know'
    yield ('{"agent":{"binding":{"type":"multi_session_stdio"}},"provider":{"binding":{"type":"raw_stdio"}}}',
           'exclusive use of stdio')
    yield ('{"provider":{"binding":{"type":"http","address":"127.0.0.1:8000"}},'
           '"agent":{"binding":{"type":"http","address":"localhost:08000"}}}', 'HTTP listeners conflict')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('probe', type=lambda path: str(Path(path).resolve()))
    args = parser.parse_args()
    helpers = runpy.run_path(str(Path(__file__).with_name('hub-open-directory.py')))
    with tempfile.TemporaryDirectory(prefix='service-config-') as temp:
        root = Path(temp)
        home = root / 'home'
        home.mkdir()
        config = root / 'server.json'
        env = {'HOME': str(home), 'PATH': os.defpath, 'NODE_ENV': 'test',
               'XDG_DATA_HOME': str(root / 'data'), 'XDG_CONFIG_HOME': str(root / 'config')}

        def run(document, requests=()):
            config.write_text(document if isinstance(document, str) else json.dumps(document))
            return subprocess.run([args.probe, config], cwd=root, env=env, capture_output=True, text=True,
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
        provider = {'binding': {'type': 'raw_stdio'}}
        envelope = helpers['envelope']
        capabilities = {**envelope('caps'), 'type': 'capabilities.request', 'payload': {}}
        for name, endpoint_id in [('scripted', 'reference.memory'), ('synthetic', 'codex.app-server')]:
            agent = {'adapters': adapters, 'binding': {'type': 'raw_stdio', 'adapter': name}}
            result = run({'provider': provider, 'agent': agent}, [capabilities, envelope(name)])
            assert result.returncode == 0, result
            frames = [json.loads(line) for line in result.stdout.splitlines()]
            endpoints = [frame['payload']['endpoint']['id'] for frame in frames
                         if frame['type'] == 'capabilities.response']
            assert endpoints == [endpoint_id], frames
            assert any(frame.get('payload', {}).get('session_id') == name for frame in frames), frames
            if name == 'scripted':
                assert not native.exists()
            else:
                assert 'thread/start' in native.read_text()
                native.unlink()
            print(f'raw stdio used {endpoint_id} and exited {result.returncode}')

        agent = {'adapters': adapters, 'binding': {'type': 'multi_session_stdio'}}
        requests = [{'id': index, 'op': 'open', 'adapter': name, 'request': envelope(name)}
                    for index, name in enumerate(adapters, 1)]
        result = run({'agent': agent}, requests)
        assert result.returncode == 0, result
        replies = {frame['id']: frame for line in result.stdout.splitlines()
                   if 'id' in (frame := json.loads(line))}
        for request in requests:
            helpers['opened'](replies[request['id']], request['adapter'])
        methods = [json.loads(line)['request']['method'] for line in native.read_text().splitlines()]
        assert methods.count('thread/start') == 1, result
        print(f'multi-session opened {len(requests)} sessions and exited {result.returncode}')

        result = run({'provider': {'binding': {'type': 'http', 'address': '127.0.0.1:6271'}}})
        assert result.returncode == 0 and not result.stdout, result
        print(f'provider-only configuration validation exited {result.returncode}')
        print(f'PASS: {invalid_count} refusals, 2 raw sessions, {len(requests)} multi-session opens, 1 provider-only config')


if __name__ == '__main__':
    main()
