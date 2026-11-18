# Catalog request headers

With the shared setup in `SKILL.md`, run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/catalog-request-headers.py /tmp/oap-catalog-request-headers-<unique-name>
```

Require exit 0 and `PASS`. The script builds the current source with a temporary
catalog fixture and drives a served turn against a loopback provider. It checks
literal and environment-backed headers, a file override, one User-Agent, and
that a missing environment value leaves other providers usable, and that an
accepted inference fails without stopping the provider endpoint. It retains the
build log and protocol transcripts; the synthetic environment value must not
appear there.
