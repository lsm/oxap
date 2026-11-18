# Go provider catalog headers

With the shared setup in `SKILL.md`, run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/go-catalog-headers.py /tmp/oap-go-catalog-headers-<unique-name>
```

Require exit 0 and `PASS`. A standalone Go consumer drives the actual provider
discovery and inference runtime against a loopback server. It checks literal and
environment-backed headers on both requests, a single User-Agent, refusal when
the environment value is missing, and that inference events do not expose it.
The provider credential exists only in the consumer's memory.
