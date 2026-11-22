import json
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory


def check(binary):
    return subprocess.run(
        [binary, "check"], capture_output=True, text=True, timeout=60,
        env={"NODE_ENV": "test"},
    )


def main():
    versions = json.loads(Path("harnesses/pi.json").read_text())["versions"]
    pin = next(version for version in versions if version["status"] == "current")
    with TemporaryDirectory(prefix="rust-pin-check-") as build:
        binary = Path(build) / "goap"
        subprocess.run(
            ["go", "build", "-o", binary, "./go/cmd/goap"], check=True, timeout=120
        )
        with TemporaryDirectory(dir="rust", prefix="pin-probe-") as directory:
            probe = Path(directory) / "probe.rs"
            probe.write_text(
                f'const REVISION: &str = "{pin["capability_revision"]}";\n'
                f'const CORPUS: &str = r#"{pin["corpus"]}"#;\n'
            )
            rejected = check(binary)
            diagnostics = [
                line for line in rejected.stderr.splitlines() if "pin_literal:" in line
            ]
            print(f"Pin probe exit {rejected.returncode}, {len(diagnostics)} pin diagnostics")
            for line in diagnostics:
                print(line)
            expected = [
                f'{probe}:1 spells the catalog\'s capability_revision "{pin["capability_revision"]}"',
                f'{probe}:2 spells the catalog\'s corpus "{pin["corpus"]}"',
            ]
            if rejected.returncode != 1 or len(diagnostics) != 2 or not all(
                detail in line for detail, line in zip(expected, diagnostics)
            ):
                raise SystemExit("Expected both Rust pin diagnostics.\n" + rejected.stderr)
        clean = check(binary)
        passes = clean.stdout.splitlines().count("PASS check")
        print(f"Clean check exit {clean.returncode}, {passes} PASS check")
        if clean.returncode != 0 or passes != 1:
            raise SystemExit(clean.stdout + clean.stderr)
    print("PASS: 2 checks")


if __name__ == "__main__":
    main()
