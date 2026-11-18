# Served oapx stdio MCP tools

Build ReleaseSafe with the shared setup in `SKILL.md`, then run from the repository root:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-stdio.py /tmp/oap-mcp-stdio-<unique-name>
```

Require exit 0 and `PASS`. The isolated hub uses a synthetic loopback provider and a fake stdio MCP server. Inspect `mcp.jsonl` and `provider-requests.json` for discovery, attached-source metadata, a provided tool attributed to that source, a tool call, and its returned text. The journey also checks an unsupported source refusal, validates a bad model before starting the server, and observes the server exit on stdin EOF. No live credentials or external providers are used.

The adapter CI group also checks that source descriptions survive reuse of the open-request buffer.
