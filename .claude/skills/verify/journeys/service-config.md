# Service configuration and Agent adapter selection

Use the isolation in `SKILL.md` and run the journey on the CI merge candidate
or the same tree on a build host.

```sh
zig build --build-file zig/build.zig build-service-config-probe -Doptimize=ReleaseSafe
python3 .claude/skills/verify/scripts/service-config.py zig/zig-out/bin/service-config-probe
go test ./go/validation -run '^TestServiceConfigSchema$' -count=1 -v
```

The probe reads YAML and JSON documents and starts the Agent through its config loader and production binding
loops. It stays outside the installed CLI until config-based dispatch ships.
The journey opens sessions through multi-session and raw stdio using a single
registry definition with the memory adapter and a synthetic Codex peer. It
also lists both adapters over loopback HTTP and checks the Host allowlist. It
requires invalid configs to exit 1 with a diagnostic, no listener output,
and no state created in the isolated home or data directories. The provider
cases exercise configuration validation only. The journey reports the number
of refusals, sessions and successful exits.

The schema check uses the repository validator and shares the invalid config
fixtures with the probe. Both refuse invalid port syntax, unrelated adapter
fields, and literal tool-source auth values.
