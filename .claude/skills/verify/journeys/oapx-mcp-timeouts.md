# Timed-out MCP servers

Build ReleaseSafe with the shared setup in `SKILL.md`, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-timeouts.py /tmp/oap-mcp-timeouts-<unique-name>
```

Require exit 0 and `PASS`. An isolated hub opens four sessions against a
synthetic server that accepts requests without replying. Each open must fail,
and the hub must not accumulate a worker thread for each timeout.
