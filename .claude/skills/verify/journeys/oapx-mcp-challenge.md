# MCP authorization challenge transport

Build the probe and run it against the fake MCP server:

```sh
zig build --build-file zig/build.zig mcp-challenge-probe -Doptimize=ReleaseSafe
NODE_ENV=test python3 .claude/skills/verify/scripts/mcp-challenge.py zig/zig-out/bin/mcp-challenge-probe
```

Require exit 0, one request and `PASS: 401 challenge retained`. This checks
the transport boundary only; it does not prove OAuth login or tool discovery.
