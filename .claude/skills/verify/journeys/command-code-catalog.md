# Command Code catalog

Build ReleaseSafe using the shared setup in `SKILL.md`, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/command-code-catalog.py /tmp/oap-command-code-<unique-name>
```

Require exit 0 and `PASS`. A served session discovers a loopback catalog,
excludes models without Chat Completions support, and completes a chat turn.
The loopback provider checks the zero-data-retention and User-Agent headers on
discovery and inference. No provider key is present in the environment.
