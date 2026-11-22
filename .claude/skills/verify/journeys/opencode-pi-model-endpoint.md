# OpenCode, Pi, Copilot and Docker Agent custom endpoints

Follow the shared Verify setup and run from the repository root.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
python3 .claude/skills/verify/scripts/opencode-pi-model-endpoint.py --binary "$EVIDENCE/oxap/bin/oxap" --goap "$EVIDENCE/goap"
```

The Zig CI hub parity job runs the same journey. The probe opens sessions and
completes turns against native fixtures. It checks OpenCode's inline config
and session model, Pi's generated file and model flags, and the existing
environment forwarding for Copilot and Docker Agent. A loopback endpoint checks
the requested path and authentication for each wire. OpenCode and Pi fixture
output must replace the generated sentinel with its variable name. The probe
also checks Pi stderr and named startup refusals.
