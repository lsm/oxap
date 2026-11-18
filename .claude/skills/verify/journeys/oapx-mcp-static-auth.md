# Configured MCP headers and bearer token

Build ReleaseSafe with the shared setup, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-static-auth.py "$EVIDENCE/mcp-static-auth"
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-oauth-login.py zig/zig-out/bin/oxap static
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-oauth-login.py zig/zig-out/bin/oxap static-rejected
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-oauth-login.py zig/zig-out/bin/oxap oauth-header
```

Require exit 0 and `PASS` for literal and environment-backed values. The
fake server checks headers on initialization, tool discovery, the tool call,
SSE ping response and session deletion. Missing or empty header or token variables
must refuse startup with zero MCP requests. The journey checks the wire log
and diagnostics for credential values and confirms authenticated redirects
are not followed.
The final runs check that a configured bearer overrides saved OAuth, a rejected
static bearer does not trigger OAuth discovery or refresh, and
that other configured headers accompany OAuth requests and refresh retries.
