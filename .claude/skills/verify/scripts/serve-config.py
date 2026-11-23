import http.client
import http.server
import json
import os
from pathlib import Path
import runpy
import select
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time


AGENT = 'open-agent-protocol.agent-control-core'
PROVIDER = 'open-agent-protocol.model-provider-core'


def envelope(profile, kind, ident='same', payload=None):
    return {'protocol': 'open-agent-protocol', 'version': '0.1', 'profile': profile,
            'type': kind, 'id': ident, 'payload': payload or {}}


def command_for(binary, root):
    if sys.platform != 'darwin':
        return [binary, 'serve', '--config']
    profile = root / 'isolation.sb'
    profile.write_text(
        '(version 1) (allow default) (deny network*)\n'
        '(allow network-outbound (remote ip "localhost:*"))\n'
        '(allow network-bind (local ip "localhost:*"))\n'
        '(allow network-inbound (local ip "localhost:*"))\n'
        '(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n'
        f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n'
    )
    return ['/usr/bin/sandbox-exec', '-f', profile, binary, 'serve', '--config']


def free_port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def http_request(port, path, frame=None, retry=False):
    until = time.monotonic() + 10
    while True:
        connection = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
        try:
            if frame is None:
                connection.request('GET', path)
            else:
                connection.request('POST', path, json.dumps(frame), {'Content-Type': 'application/json'})
            response = connection.getresponse()
            assert response.status == 200, (response.status, response.reason)
            return json.load(response)
        except ConnectionRefusedError:
            if not retry or time.monotonic() >= until:
                raise
            time.sleep(0.05)
        finally:
            connection.close()


def send(process, frame):
    process.stdin.write(json.dumps(frame) + '\n')
    process.stdin.flush()
    assert select.select([process.stdout], [], [], 12)[0], 'missing stdio response'
    return json.loads(process.stdout.readline())


def assert_response(reply, request):
    assert (reply['profile'], reply['in_reply_to'], reply['type']) == (
        request['profile'], request['id'], request['type'].replace('.request', '.response')
    ), reply


def mixed(start, agent, provider, capabilities, describe):
    count = 0
    path = '/oap/v0.1/provider'
    for agent_mode, provider_mode in [
        (None, 'http'), ('http', 'raw'), ('raw', 'http'), ('multi_session', 'http'), ('http', 'http')
    ]:
        provider_port = free_port() if provider_mode == 'http' else None
        agent_port = free_port() if agent_mode == 'http' else None
        while agent_port is not None and agent_port == provider_port:
            agent_port = free_port()
        selected_agents = {
            'raw': agent,
            'multi_session': {'stdio': 'multi_session', 'adapters': agent['adapters']},
            'http': {'http': f'127.0.0.1:{agent_port}', 'adapters': agent['adapters']},
        }
        document = {'provider': provider if provider_port is None else {'http': f'127.0.0.1:{provider_port}'}}
        if agent_mode:
            document['agent'] = selected_agents[agent_mode]
        with start(document) as process:
            try:
                describe_reply = (http_request(provider_port, path, describe, retry=True) if provider_port
                                  else send(process, describe))
                assert_response(describe_reply, describe)
                if provider_port:
                    assert all(model['credential_grant'] == 'none'
                               for model in describe_reply['payload']['providers']), describe_reply
                count += 1
                if agent_mode == 'http':
                    listed = http_request(agent_port, '/adapters', retry=True)
                    assert [e['name'] for e in listed['adapters']] == ['scripted'], listed
                elif agent_mode == 'raw':
                    assert_response(send(process, capabilities), capabilities)
                elif agent_mode == 'multi_session':
                    request = envelope(AGENT, 'session.open.request', 'open', {'session_id': 'http-peer'})
                    opened = send(process, {'id': 1, 'op': 'open', 'adapter': 'scripted', 'request': request})
                    assert opened.get('result', {}).get('payload', {}).get('session_id') == 'http-peer', opened
                count += int(agent_mode is not None)
                if agent_mode not in ['raw', 'multi_session'] and provider_mode != 'raw':
                    process.send_signal(signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=12)
                assert process.returncode == 0, (agent_mode, provider_mode, process.returncode, stdout, stderr)
                if agent_mode != 'http' or provider_mode == 'raw':
                    assert not stdout, stdout
                print(f'Agent {agent_mode}, Provider {provider_mode} settled {1 + int(agent_mode is not None)} requests and exited {process.returncode}')
            finally:
                if process.poll() is None:
                    process.kill()
    return count


def inference_journeys(run, start, home, provider):
    fixture = runpy.run_path(str(Path(__file__).with_name('oapx-session-restart.py')))
    upstream_type = fixture['Provider']
    model_ref = 'synthetic/openai-chat-completions@synthetic-model'
    peer = home / 'peer.py'
    peer.write_text(runpy.run_path(str(Path(__file__).with_name('hub-open-directory.py')))['PEER'])
    agent = {'stdio': 'raw', 'adapter': 'synthetic', 'adapters': {
        'synthetic': {'type': 'codex', 'executable': sys.executable,
                      'args': [str(peer), str(home / 'peer.jsonl')],
                      'working_directory': str(home), 'model': 'synthetic-model',
                      'approval_policy': 'on-request', 'sandbox': 'workspace-write'}
    }}
    state = home / '.oxap'
    state.mkdir(exist_ok=True)
    models = {'providers': [{'id': 'synthetic', 'api': 'openai-completions', 'auth': 'none',
                            'models': ['synthetic-model']}]}
    with http.server.ThreadingHTTPServer(('127.0.0.1', 0), upstream_type) as upstream:
        models['providers'][0]['base_url'] = f'http://127.0.0.1:{upstream.server_port}/v1'
        (state / 'providers.json').write_text(json.dumps(models))
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        try:
            session = 'configured-run'
            open_session = envelope(AGENT, 'session.open.request', 'open', {'session_id': session})
            user = [{'role': 'user', 'content': 'configured services proof'}]
            submit = envelope(AGENT, 'session.message.submit.request', 'turn', {
                'session_id': session, 'delivery': 'auto', 'messages': user
            })
            submit['session_id'] = session
            inference = envelope(PROVIDER, 'inference.create.request', 'turn', {
                'model_ref': model_ref, 'stream': True, 'messages': user
            })
            result = run({'agent': agent, 'provider': provider}, open_session, submit, inference)
            assert result.returncode == 0, result
            frames = [json.loads(line) for line in result.stdout.splitlines()]
            for profile, terminal in [(AGENT, 'run.completed'), (PROVIDER, 'inference.completed')]:
                assert sum(r['profile'] == profile and r['type'] == terminal for r in frames) == 1, frames
            print(f'dual raw stdio completed one run and one inference after EOF, exit {result.returncode}')

            port = free_port()
            built_in = {'stdio': 'raw', 'adapter': 'loop', 'provider': 'cohosted',
                        'adapters': {'loop': {'type': 'oapx'}}}
            auth = envelope(AGENT, 'provider.credential.grant.request', 'grant',
                            {'provider_id': 'synthetic', 'nonce': 'agent', 'ttl_ms': 1000})
            models_query = envelope(AGENT, 'models.request', 'models', {
                'session_id': session
            })
            models_query['session_id'] = session
            bound_provider = {'http': f'127.0.0.1:{port}'}
            direct = {key: value for key, value in built_in.items() if key != 'provider'}
            result = run({'agent': direct, 'provider': bound_provider}, open_session, models_query)
            assert result.returncode == 0, result
            reply = json.loads(result.stdout.splitlines()[-1])
            assert_response(reply, models_query)
            direct_ids = {entry['id'] for entry in reply['payload']['models']}
            assert model_ref.replace('chat-completions', 'completions') in direct_ids, reply
            assert model_ref not in direct_ids, reply

            with start({'agent': built_in, 'provider': bound_provider}) as process:
                try:
                    assert_response(send(process, open_session), open_session)
                    reply = send(process, models_query)
                    assert_response(reply, models_query)
                    assert model_ref in {entry['id'] for entry in reply['payload']['models']}, reply
                    grant_reply = send(process, auth)
                    assert grant_reply['profile'] == AGENT and grant_reply['type'] == 'error.response', grant_reply
                    stdout, stderr = process.communicate(json.dumps(submit) + '\n', timeout=12)
                    assert process.returncode == 0, (process.returncode, stdout, stderr)
                    frames = [json.loads(line) for line in stdout.splitlines()]
                    assert sum(frame['type'] == 'run.completed' for frame in frames) == 1, frames
                    assert fixture['REPLY'] in stdout, stdout
                finally:
                    if process.poll() is None:
                        process.kill()
            assert len(upstream_type.requests) == 2, len(upstream_type.requests)
            print('Agent retained its catalog by default and used the Provider catalog and inference after explicit selection')
            print('Agent profile refused a Provider grant request')
        finally:
            upstream.shutdown()
            thread.join()


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
        command = command_for(binary, root)
        env = {'HOME': str(home), 'PATH': os.defpath, 'NODE_ENV': 'test',
               'XDG_DATA_HOME': str(root / 'data'), 'XDG_CONFIG_HOME': str(root / 'config')}

        def run(document, *requests):
            config.write_text(json.dumps(document))
            return subprocess.run(
                [*command, config], cwd=root, env=env,
                input=''.join(json.dumps(frame) + '\n' for frame in requests),
                text=True, capture_output=True, timeout=20,
            )

        def start(document):
            config.write_text(json.dumps(document))
            return subprocess.Popen([*command, config], cwd=root, env=env, stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

        built_in = {'stdio': 'raw', 'adapter': 'loop', 'adapters': {'loop': {'type': 'oapx'}}}
        auth = envelope(AGENT, 'auth.providers.request', 'auth')
        count = 0
        for document, requests in [
            ({'agent': built_in, 'provider': provider}, [auth, describe]),
            ({'agent': built_in}, [auth]),
        ]:
            result = run(document, *requests)
            assert result.returncode == 0, result
            frames = [json.loads(line) for line in result.stdout.splitlines()]
            assert len(frames) == len(requests), frames
            replies = {reply['in_reply_to']: reply for reply in frames}
            for request in requests:
                assert_response(replies[request['id']], request)
            assert replies['auth']['payload']['providers'], replies['auth']
            count += len(replies)
            print(f'{list(document)} built-in Agent discovered auth providers, answered {len(replies)} requests and exited {result.returncode}')

        conflict = run({'agent': {'stdio': 'multi_session'}, 'provider': provider})
        assert conflict.returncode == 1 and not conflict.stdout, conflict
        assert 'exclusive use of stdio' in conflict.stderr, conflict
        assert not list(home.iterdir()), conflict
        print(f'conflicting stdio bindings exited {conflict.returncode}')

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
        missing_profile = {key: value for key, value in capabilities.items() if key != 'profile'}
        wrong_type = envelope(AGENT, 'provider.describe.request', 'wrong-type')
        result = run({'agent': agent, 'provider': provider}, missing_profile, wrong_type, describe)
        assert result.returncode == 0, result
        rejected, mismatch, answer = [json.loads(line) for line in result.stdout.splitlines()]
        assert rejected['profile'] == AGENT and rejected['in_reply_to'] == 'same', rejected
        assert rejected['payload']['error']['code'] == 'invalid_request', rejected
        assert rejected['payload']['error']['details']['feature'] == 'profile', rejected
        assert mismatch['profile'] == AGENT and mismatch['type'] == 'error.response', mismatch
        assert_response(answer, describe)

        for broken, diagnostic in [
            ('malformed', 'stdin carried a line that is not an OAP envelope'),
            ({key: value for key, value in capabilities.items() if key != 'id'},
             'stdin carried an envelope with no id'),
        ]:
            result = run({'agent': agent, 'provider': provider}, capabilities, broken)
            assert result.returncode == 1, result
            assert_response(json.loads(result.stdout), capabilities)
            assert diagnostic in result.stderr, result
        print('composed stdio kept the prior reply and explained two fatal framing failures')

        invalid_selection = run({'agent': {**agent, 'provider': 'cohosted'}, 'provider': provider})
        assert invalid_selection.returncode == 1 and not invalid_selection.stdout, invalid_selection
        assert 'cohosted requires provider.http' in invalid_selection.stderr, invalid_selection
        count += 3 + mixed(start, agent, provider, capabilities, describe)
        inference_journeys(run, start, home, provider)
        print(f'PASS: two config refusals, {count} discovery and routing requests, two runs and one inference settled')


if __name__ == '__main__':
    main()
