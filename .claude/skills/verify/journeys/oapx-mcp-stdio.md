# Served oapx stdio MCP tools

Build ReleaseSafe with the shared setup in `SKILL.md`, then run from the repository root:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-stdio.py /tmp/oap-mcp-stdio-<unique-name>
```

Require exit 0 and `PASS`. The isolated hub uses a synthetic loopback provider and a fake stdio MCP server. Inspect `mcp.jsonl` and `provider-requests.json` for discovery, a tool call, and its returned text. No live credentials or external providers are used.
