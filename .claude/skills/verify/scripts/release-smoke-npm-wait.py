import subprocess
import sys
import tempfile
import time
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[4]
WORKFLOW = ROOT / ".github/workflows/release-smoke.yml"
FAKE_NPM = """#!/bin/sh
count=$(cat "$NPM_CALLS" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" > "$NPM_CALLS"
sleep "$NPM_DELAY"
[ "$count" -gt "$NPM_FAIL_CALLS" ]
"""
TIMEOUT = """#!/bin/sh
exec perl -e 'alarm shift; exec @ARGV or die' "$@"
"""


def step():
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else WORKFLOW
    jobs = yaml.safe_load(path.read_text())["jobs"]
    for s in jobs["npm"]["steps"]:
        if s.get("id") == "published":
            return s["run"]
    raise SystemExit("no published step in the npm job")


def run(script, fail_calls, wait, on_pr, delay=0):
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        for name, body in (("npm", FAKE_NPM), ("timeout", TIMEOUT)):
            (bin_dir / name).write_text(body)
            (bin_dir / name).chmod(0o755)
        out = tmp / "github_output"
        out.touch()
        env = {
            "PATH": f"{bin_dir}:/usr/bin:/bin",
            "VERSION": "0.0.0-test",
            "ON_PULL_REQUEST": "true" if on_pr else "false",
            "WAIT_SECONDS": str(wait),
            "RETRY_SECONDS": "1",
            "GITHUB_OUTPUT": str(out),
            "NPM_CALLS": str(tmp / "calls"),
            "NPM_FAIL_CALLS": str(fail_calls),
            "NPM_DELAY": str(delay),
        }
        start = time.monotonic()
        p = subprocess.run(["bash", "-e", "-c", script], env=env, capture_output=True, text=True)
        elapsed = time.monotonic() - start
        calls = int((tmp / "calls").read_text())
        return p.returncode, out.read_text().strip(), calls, p.stdout, elapsed


def main():
    script = step()
    never = 10**6
    cases = [
        ("late publish inside the window", dict(fail_calls=6, wait=20, on_pr=False), 0, "published=true", 20),
        ("never published on a release", dict(fail_calls=never, wait=2, on_pr=False), 1, "published=false", 2),
        ("never published on a pull request", dict(fail_calls=never, wait=2, on_pr=True), 0, "published=false", 2),
        ("slow registry on a release", dict(fail_calls=never, wait=2, on_pr=False, delay=5), 1, "published=false", 2),
    ]
    failed = 0
    for name, kw, want_exit, want_out, budget in cases:
        code, out, calls, stdout, elapsed = run(script, **kw)
        ok = code == want_exit and out == want_out and elapsed < budget + 1.5
        if name.startswith("late"):
            ok = ok and "not on npm yet" in stdout and calls == 12
        failed += not ok
        print(f"{'PASS' if ok else 'FAIL'} {name}: exit={code} output={out} npm_calls={calls} elapsed={elapsed:.2f}s budget={budget}s")
    print(f"failed={failed}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
