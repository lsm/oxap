# MCP OAuth metadata discovery

Build the product and probe with the shared setup, then run:

```sh
zig build --build-file zig/build.zig mcp-oauth-discovery-probe -Doptimize=ReleaseSafe
NODE_ENV=test python3 .claude/skills/verify/scripts/mcp-oauth-metadata.py zig/zig-out/bin/mcp-oauth-discovery-probe
NODE_ENV=test python3 .claude/skills/verify/scripts/oapx-mcp-oauth-discovery.py "$EVIDENCE/mcp-oauth-discovery"
```

Require both scripts to exit 0 and print `PASS`. The served journey checks a
401 challenge and both metadata fetches. The probe checks issuer matching,
transport policy and the response-size limit against a synthetic server.
Login and token use are not yet implemented.
