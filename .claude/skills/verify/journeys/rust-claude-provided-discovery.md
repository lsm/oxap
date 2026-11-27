# Rust Claude provided tool discovery

Follow shared isolation and mutex setup in `SKILL.md`. Run from the repository
root on a test host with Python 3.11 or newer.

```sh
bash .claude/skills/verify/scripts/rust-claude-provided-discovery.sh
```

Require exit 0 and three passing cases. The Rust adapter opens a credential-free
stdio peer through its staged provisioning path, as in the Go provided-tool
fixtures. The peer checks the exact allowlist names and discovers the SDK MCP
server before it acknowledges the host's initialize request. Check discovery
versions, ID echo, notification acknowledgements, unknown methods and unknown
servers. The scoped OAP catalog must expose each native echo under its supplied
name, owner, schema and source. A bare native name on either side of its SDK
echo must yield a single entry with the provided definition in the first
position. Both launch postures and the empty provision case must reach EOF on
close.

Invalid definitions must return a typed refusal before spawning. Public open
must keep the same capability gate as Go while provisioning is staged.
