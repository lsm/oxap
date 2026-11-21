# Served oapx parking default

Build ReleaseSafe with the shared setup in `SKILL.md`, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-park-default.py /tmp/oap-park-default-<unique-name>
```

Require exit 0 and `PASS`. Two isolated hubs each serve one memory-adapter
session through one turn. With no flag, the session must still be unparked at
three idle seconds and parked by six and a half. With `--park-after 0`, it must
not be parked at either point. A park is read through the `events` op, which
answers a cursor from before the park with `oap-replay-gap` and does not wake
the session. Inspect `results.json` for both outcomes. No model provider,
network or credentials are used.

CI runs this journey in `ci-zig.yml`.
