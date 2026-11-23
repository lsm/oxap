import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


AGENT = 'open-agent-protocol.agent-control-core'
PROVIDER = 'open-agent-protocol.model-provider-core'


def envelope(profile, kind, ident='same', payload=None):
    return {'protocol': 'open-agent-protocol', 'version': '0.1', 'profile': profile,
            'type': kind, 'id': ident, 'payload': payload or {}}


def main():
    binary = Path(sys.argv[1]).resolve()
    agent = {'stdio': 'raw', 'adapter': 'scripted', 'adapters': {'scripted': {'type': 'memory'}}}
    provider = {'stdio': 'raw'}
    capabilities = envelope(AGENT, 'capabilities.request')
    describe = envelope(PROVIDER, 'provider.describe.request')
    with tempfile.TemporaryDirectory(prefix='serve-config-') as temporary:
        root = Path(temporary)
        config = root / 'server.json'
        home = root / 'home'
        home.mkdir()
        env = {'HOME': str(home), 'PATH': os.defpath, 'NODE_ENV': 'test',
               'XDG_DATA_HOME': str(root / 'data'), 'XDG_CONFIG_HOME': str(root / 'config')}

        def run(document, *requests):
            config.write_text(json.dumps(document))
            return subprocess.run(
                [binary, 'serve', '--config', config], cwd=root, env=env,
                input=''.join(json.dumps(frame) + '\n' for frame in requests),
                text=True, capture_output=True, timeout=20,
            )

        conflict = run({'agent': {'stdio': 'multi_session'}, 'provider': provider})
        assert conflict.returncode == 1 and not conflict.stdout, conflict
        assert 'exclusive use of stdio' in conflict.stderr, conflict
        assert not list(home.iterdir()), conflict
        print(f'conflicting stdio bindings exited {conflict.returncode}')

        count = 0
        for document, requests in [
            ({'agent': agent}, [capabilities]),
            ({'provider': provider}, [describe]),
            ({'agent': agent, 'provider': provider}, [capabilities, describe]),
            ({'provider': provider, 'agent': agent}, [describe, capabilities]),
        ]:
            result = run(document, *requests)
            assert result.returncode == 0, result
            replies = [json.loads(line) for line in result.stdout.splitlines()]
            assert sorted((r['profile'], r['in_reply_to'], r['type']) for r in replies) == sorted(
                (r['profile'], r['id'], r['type'].replace('.request', '.response')) for r in requests
            ), replies
            count += len(replies)
            print(f'{list(document)} raw stdio settled {len(replies)} requests and exited {result.returncode}')
        print(f'PASS: one conflict and {count} correlated replies')


if __name__ == '__main__':
    main()
