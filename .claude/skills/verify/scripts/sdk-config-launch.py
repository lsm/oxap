import json
from pathlib import Path
import sys


def prepare(directory, binary):
    directory.mkdir()
    wrapper = directory / 'oxap'
    wrapper.write_text(f'#!{sys.executable}\n' + f'''import json
import os
from pathlib import Path
import sys

binary = {str(binary.resolve())!r}
if len(sys.argv) != 4 or sys.argv[1:3] != ['serve', '--config']:
    sys.exit('SDK must pass serve --config <path>')
config = Path(sys.argv[3])
document = json.loads(config.read_text())
assert document['agent']['stdio'] == document['provider']['stdio'] == 'raw'
assert document['agent']['adapters'][document['agent']['adapter']]['type'] == 'oapx'
with (Path(__file__).parent / 'launches.jsonl').open('a') as report:
    report.write(json.dumps(str(config)) + '\\n')
os.execv(binary, [binary, *sys.argv[1:]])
''')
    wrapper.chmod(0o700)


def check(directory):
    launches = (directory / 'launches.jsonl').read_text().splitlines()
    paths = {Path(json.loads(line)) for line in launches}
    assert launches and len(paths) == len(launches), 'expected a unique config per runtime'
    assert all(not path.exists() for path in paths), 'SDK left a config file behind'
    print(f'{len(launches)} configured SDK children launched, no config files left')


if __name__ == '__main__':
    command, directory, *rest = sys.argv[1:]
    if command == 'prepare':
        prepare(Path(directory), Path(rest[0]))
    else:
        assert command == 'check'
        check(Path(directory))
