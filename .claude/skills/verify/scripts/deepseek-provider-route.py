import argparse
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import uuid


ROOT = Path(__file__).resolve().parents[4]
FIXTURE = ROOT / "go/cmd/goap/testdata/parity/deepseek"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve()
    opening = json.loads((FIXTURE / "scenario.jsonl").read_text().splitlines()[2])

    with tempfile.TemporaryDirectory(prefix="deepseek-provider-route-") as directory:
        root = Path(directory).resolve()
        home = root / "home"
        home.mkdir()
        child = root / "child.sh"
        child.write_text((FIXTURE / "child.sh").read_text().replace(
            "#!/bin/sh\n",
            r"""#!/bin/sh
fail() { printf '%s\n' "$1" > route-failure; exit 1; }
[ "$DEEPSEEK_BASE_URL" = 'https://opencode.ai/inference/go' ] || fail 'endpoint mismatch'
[ "$DEEPSEEK_API_KEY" = 'fixture-route' ] || fail 'credential mismatch'
! /usr/bin/printenv OPENCODE_API_KEY >/dev/null || fail 'hub credential name leaked'
[ "$DSH_HOME" = "$PWD/home" ] || fail 'entry environment missing'
""",
        ))
        child.chmod(0o700)
        entry = json.loads((FIXTURE / "registry.json").read_text().replace("@DIR@", str(root)))["adapters"]["deepseek"]
        entry.update(model_provider="opencode-go", provider="deepseek-official", model="deepseek-v4-pro")
        entry["environment"].append(f"DSH_HOME={home}")
        registry = root / "registry.json"
        env = {"PATH": os.defpath, "HOME": str(home), "NODE_ENV": "test",
               "OXAP_KEYCHAIN_SERVICE": f"oxap-verify-{uuid.uuid4()}"}
        cmd = [str(binary), "serve", "--stdio", "--no-save-oapx-sessions", "--config", str(registry)]

        registry.write_text(json.dumps({"adapters": {"routed": entry}}))
        stderr_path = root / "stderr"
        with stderr_path.open("w") as stderr, subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
            env={**env, "OPENCODE_API_KEY": "fixture-route"}, bufsize=0,
        ) as hub:
            try:
                hub.stdin.write((json.dumps({"id": 1, "op": "open", "adapter": "routed", "request": opening}) + "\n").encode())
                for _ in range(10):
                    assert select.select([hub.stdout], [], [], 10)[0], "hub open timed out"
                    line = hub.stdout.readline()
                    assert line, stderr_path.read_text()
                    reply = json.loads(line)
                    if reply.get("id") == 1:
                        break
                failure = root / "route-failure"
                assert reply.get("id") == 1 and "error" not in reply, (
                    failure.read_text().strip() if failure.exists() else reply
                )
                assert reply["result"]["type"] == "session.open.response", reply
                hub.stdin.close()
                assert hub.wait(timeout=10) == 0, hub.returncode
            finally:
                if hub.poll() is None:
                    hub.kill()
                    hub.wait(timeout=10)

        frames = [json.loads(line) for line in (root / "stdin.log").read_text().splitlines() if line.startswith("{")]
        starts = [frame["params"] for frame in frames if frame["method"] == "initialize"]
        assert len(starts) == 1, starts
        assert starts[0]["provider"] == "deepseek-official", starts
        assert starts[0]["model"] == "deepseek-v4-pro", starts
        print(f"hub exit={hub.returncode}, dsh initializes={len(starts)}")

        refusals = [
            ({"model_provider": "no-such-row"}, "which is no catalog provider"),
            ({"model_provider": "openrouter"}, "serves no anthropic-messages endpoint"),
            ({}, "which has no credential; set OPENCODE_API_KEY"),
            ({"environment": ["DEEPSEEK_BASE_URL=https://example.test"]}, 'sets DEEPSEEK_BASE_URL in "environment"'),
            ({"environment": ["DEEPSEEK_API_KEY="]}, 'sets DEEPSEEK_API_KEY in "environment"'),
        ]
        for override, diagnostic in refusals:
            registry.write_text(json.dumps({"adapters": {"routed": {**entry, **override}}}))
            result = subprocess.run(cmd, input="", capture_output=True, text=True, env=env, timeout=15)
            assert result.returncode != 0 and not result.stdout, result
            assert '"routed"' in result.stderr and diagnostic in result.stderr, result.stderr
            print(f"hub exit={result.returncode}, {diagnostic}")
        print(f"PASS: one routed DeepSeek session and {len(refusals)} named startup refusals")


if __name__ == "__main__":
    main()
