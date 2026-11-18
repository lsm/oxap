# Served oapx Streamable HTTP MCP tools

Build ReleaseSafe with the shared setup in `SKILL.md`, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-http.py /tmp/oap-mcp-http-<unique-name>
```

Require exit 0 and `PASS`. The isolated hub uses a synthetic loopback model
provider and a stateful Streamable HTTP MCP server. It checks initialization,
session propagation, JSON and SSE replies, an inbound ping while the stream is
open, tool discovery and execution, and session deletion. Inspect `http.jsonl`
for the served exchange. No credentials or live providers are used.

CI also runs this journey against a stateless server and over HTTPS. The HTTPS
run uses a temporary certificate trusted only on its disposable runner.
