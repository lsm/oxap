# Launch the SDKs with a service config

Use the isolation in `SKILL.md`. Build `oxap` in the candidate tree, then
prepare a verification wrapper outside the checkout before each SDK suite:

```sh
zig build --build-file zig/build.zig -Doptimize=ReleaseSafe
python3 .claude/skills/verify/scripts/sdk-config-launch.py prepare "$EVIDENCE/sdk-launch" zig/zig-out/bin/oxap
export OAP_SDK_BINARY_PATH="$EVIDENCE/sdk-launch/oxap"
```

Use a fresh wrapper directory for each SDK. The Go, Rust, TypeScript and
Python jobs in CI show the full suite commands and enable the real-binary
cases. They require successful Agent initialize and Provider describe.
The wrapper checks that each default launch uses a readable JSON file with
raw Agent and Provider bindings, then execs the real binary on the same pipe.
After the suite exits, run:

```sh
python3 .claude/skills/verify/scripts/sdk-config-launch.py check "$EVIDENCE/sdk-launch"
```

Record the suite exit status, executed and skipped tests, and the count of
configured children with their config files removed. The TypeScript
`clients/ts` integration uses `OAP_TS_AGENT_SERVICE` pointing at the real
binary to open and subscribe a session over the configured HTTP Agent
Service and verify its exit status. Run the auth smoke cases on Linux CI.
