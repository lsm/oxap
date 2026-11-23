# Service configuration and Agent adapter selection

Use the isolation in `SKILL.md` and run the journey on the CI merge candidate
or the same tree built on a build host.

```sh
zig build --build-file zig/build.zig -Doptimize=ReleaseSafe
python3 .claude/skills/verify/scripts/service-config.py zig/zig-out/bin/oxap
python3 .claude/skills/verify/scripts/serve-config.py zig/zig-out/bin/oxap
go test ./go/validation -run '^TestServiceConfigSchema$' -count=1 -v
```

On macOS copy the built binary to a temporary directory outside your home
before running `serve-config.py`. The script isolates the home and Keychain and
allows only loopback networking in the product processes.

The scripts start the installed CLI with YAML and JSON. They check that invalid
configs exit 1 before opening a listener or creating state. The Agent registry
journey opens sessions over raw and multi-session stdio using memory and a
synthetic Codex peer, and checks the HTTP Host allowlist.

The service journey covers both orderings of dual raw stdio, single-service
bindings, and all mixed HTTP/stdio bindings. It checks same-ID replies by
profile, auth provider discovery on the built-in raw Agent both alone and
alongside a raw Provider, a missing-profile refusal, fatal framing diagnostics
with a prior reply preserved, terminal events on both profiles at EOF,
and a built-in Agent turn through the explicitly selected Provider over
loopback. The test Provider is synthetic and anonymous. The HTTP Provider
discloses no grant channel and the Agent profile refuses Provider grants.
The scripts record settled counts and exit codes.

The schema check shares invalid configurations with the CLI journey.
