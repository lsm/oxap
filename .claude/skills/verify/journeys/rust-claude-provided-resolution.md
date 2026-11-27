# Rust Claude provided tool resolution

Follow shared isolation and mutex setup in `SKILL.md`. Run from the repository
root on a test host with Python 3.11 or newer.

```sh
bash .claude/skills/verify/scripts/rust-claude-provided-resolution.sh
```

Require exit 0 and three pass markers. The staged opener acknowledges a tool
whose SDK MCP request is already held, then returns a result. In a second
session, the opener returns an error before the SDK MCP request arrives.
Check the matching OAP events, resolve request IDs and exact JSON-RPC replies.
The peer sends a duplicate MCP request after receiving the reply and must get
an error. In the third session the run settles with a held MCP request. Require
an MCP cancellation error and a call cancellation before the run terminal.
For each case, verify the refused late resolution names the settlement and
session close reaches EOF.

Run `rust-claude-provided-discovery.sh` to check the public capability gate,
and `rust-claude-provided-lifecycle.sh` to check projection and tool choice.
