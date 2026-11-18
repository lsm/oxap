#!/usr/bin/env python3
"""Exercise the pinned harness installer in a temporary, isolated home."""
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[4]
BINARY = ROOT / 'zig/zig-out/bin/oxap'


def main():
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=False)
    version = next(v for v in json.loads((ROOT / 'harnesses/opencode.json').read_text())['versions'] if v['status'] == 'current')
    with tempfile.TemporaryDirectory(prefix='oxap-harness-install-') as temporary:
        root = Path(temporary).resolve()
        home = root / 'home'
        home.mkdir()
        env = {'HOME':str(home), 'PATH':'/usr/bin:/bin', 'TMPDIR':str(root), 'NODE_ENV':'test'}
        binary = root / 'oxap'
        shutil.copy2(BINARY, binary)
        command = [str(binary)]
        if sys.platform == 'darwin':
            profile = root / 'isolation.sb'
            profile.write_text('(version 1) (allow default)\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*"))\n' + f'(deny file-read* (subpath {json.dumps(str(Path.home()))}))\n')
            command = ['/usr/bin/sandbox-exec', '-f', str(profile), str(binary)]
        def run(label, args, expected):
            result = subprocess.run(command + args, cwd=root, env=env, text=True, capture_output=True, timeout=900)
            (output / (label + '.stdout')).write_text(result.stdout)
            (output / (label + '.stderr')).write_text(result.stderr)
            assert (result.returncode == 0) == expected, (label, result.returncode, result.stderr)
            return result
        help_result = run('help', ['--help'], True)
        assert 'harness install' in help_result.stdout, 'help omits harness install'
        unknown = run('unknown', ['harness','install','no-such-harness'], False)
        assert 'no-such-harness' in unknown.stderr, unknown.stderr
        refused = run('version-refusal', ['harness','install','opencode','--version','0.0.0'], False)
        assert '0.0.0' in refused.stderr and version['label'] in refused.stderr, refused.stderr
        assert not (home / '.oxap' / 'opencode').exists(), 'refusal installed a file'
        run('install', ['harness','install','opencode','--version',version['label']], True)
        installed = home / '.oxap' / 'opencode' / version['admits'][0] / 'opencode'
        assert installed.is_file() and os.access(installed, os.X_OK), 'pinned executable missing'
        installed_command = command[:-1] + [str(installed)]
        actual = subprocess.run(installed_command + ['--version'], cwd=root, env=env, capture_output=True, text=True, timeout=30)
        assert actual.returncode == 0 and version['admits'][0] in actual.stdout, (actual.returncode, actual.stdout)
        repeat = run('repeat', ['harness','install','opencode'], True)
        assert 'already installed' in repeat.stdout, repeat.stdout
        alias = run('alias', ['opencode','install'], True)
        assert 'already installed' in alias.stdout, alias.stdout
        installed.write_text('corrupted fixture')
        damaged = run('damaged', ['harness','install','opencode'], False)
        assert 'sha256' in damaged.stderr and installed.read_text() == 'corrupted fixture', damaged.stderr
        legacy_damaged = run('damaged-alias', ['opencode','install'], False)
        assert 'sha256' in legacy_damaged.stderr and installed.read_text() == 'corrupted fixture', legacy_damaged.stderr
        print('PASS: 9 checks; installed and ran the pinned harness, and refused the damaged file')

if __name__ == '__main__': main()
