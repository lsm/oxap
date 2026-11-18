# Pinned harness installation

Build ReleaseSafe with the shared setup in `SKILL.md`, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/harness-install.py /tmp/oxap-harness-install-<unique-name>
```

Require exit 0 and `PASS: 9 checks`. The journey uses an isolated temporary home and the
catalog-pinned upstream archive. It checks refusals, installs the harness,
executes its version command and checks that a repeat install reuses it. It
also corrupts the temporary copy and requires the next install to refuse it
without replacing it, through both command names. The
installation is removed with the temporary home. No provider credentials are
used.
