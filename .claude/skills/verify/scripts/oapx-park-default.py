#!/usr/bin/env python3
"""Verify that oapx serve parks an idle session after five seconds by default, and not with --park-after 0."""

import argparse
import json
import runpy
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
BENCH = runpy.run_path(str(ROOT / "scripts/bench-resources.py"))


def journey(binary, root, output, label, flags):
    work = root / label
    (work / "home").mkdir(parents=True)
    (work / "hub.json").write_text(json.dumps({"adapters": {"memory": {"type": "memory"}}}))
    environment = {"PATH": "/usr/bin:/bin", "HOME": str(work / "home"), "TMPDIR": str(work), "NODE_ENV": "test"}
    stderr = (output / f"{label}-stderr.txt").open("w")
    hub = BENCH["Hub"]([str(binary), "serve", "--stdio", "--config", str(work / "hub.json"),
                        "--session-history", str(work / "sessions.jsonl"), *flags], environment, work, stderr)
    try:
        agents = BENCH["Agents"](hub, "memory", None)
        agents.open("s0")
        run = agents.submit("s0", "hello")["payload"]["run_id"]
        agents.settle("s0")
        settled = time.monotonic()
        time.sleep(max(0.0, settled + 3 - time.monotonic()))
        early = agents.parked("s0", run)
        time.sleep(max(0.0, settled + 6.5 - time.monotonic()))
        late = agents.parked("s0", run)
        return {"parked_at_3s": early, "parked_at_6_5s": late}
    finally:
        hub.process.terminate()
        hub.process.wait(10)
        stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output_dir")
    parser.add_argument("--binary", default=str(ROOT / "zig/zig-out/bin/oxap"))
    args = parser.parse_args()
    output = Path(args.output_dir).resolve()
    output.mkdir(parents=True, exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="oap-park-default-") as temporary:
        root = Path(temporary).resolve()
        default = journey(args.binary, root, output, "default", ())
        disabled = journey(args.binary, root, output, "disabled", ("--park-after", "0"))
    report = {"default": default, "park_after_0": disabled}
    (output / "results.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report))
    assert default == {"parked_at_3s": False, "parked_at_6_5s": True}, default
    assert disabled == {"parked_at_3s": False, "parked_at_6_5s": False}, disabled
    print("PASS: an idle session parks after five seconds by default and never with --park-after 0")


if __name__ == "__main__":
    main()
