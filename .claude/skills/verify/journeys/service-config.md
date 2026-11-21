# Service configuration and Agent adapter selection

Use the isolation in `SKILL.md` and run the journey on the CI merge candidate
or the same tree on a build host.

```sh
zig build --build-file zig/build.zig build-service-config-probe -Doptimize=ReleaseSafe
python3 .claude/skills/verify/scripts/service-config.py zig/zig-out/bin/service-config-probe
```

The probe starts the Agent through its config loader and production binding
loops. It stays outside the installed CLI until config-based dispatch ships.
The journey opens sessions through multi-session and raw stdio using a single
registry definition with the memory adapter and a synthetic Codex peer. It
requires invalid configs to exit 1 with a diagnostic, no listener output,
and no state created in the isolated home or data directories. The provider
cases exercise configuration validation only. The journey reports the number
of refusals, sessions and successful exits.
