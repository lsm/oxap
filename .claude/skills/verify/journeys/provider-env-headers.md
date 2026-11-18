# Environment-backed provider headers

Build ReleaseSafe with the shared setup in `SKILL.md`, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/provider-env-headers.py /tmp/oap-provider-env-headers-<unique-name>
```

Require exit 0 and `PASS`. An isolated served session uses a synthetic loopback
provider; the server checks the resolved and literal headers without recording
header values. The journey also checks the retained artifacts and configuration
do not contain the environment value. An unset reference must refuse the turn
without sending a provider request.
