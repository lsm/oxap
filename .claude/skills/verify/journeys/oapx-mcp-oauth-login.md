# MCP OAuth browser login

Build ReleaseSafe with the shared setup, then run:

```sh
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-oauth-login.py zig/zig-out/bin/oxap challenge
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-oauth-login.py zig/zig-out/bin/oxap metadata
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-oauth-login.py zig/zig-out/bin/oxap none
```

Require exit 0 and `PASS` for both scope sources and absent scope. The fake authorization
server validates client registration, the loopback redirect, state, PKCE and
the resource parameter. A second CLI process confirms credentials persisted
without displaying them. The isolated macOS sandbox blocks Keychain access;
it does not prove the Keychain path. Token use and refresh are separate work.
