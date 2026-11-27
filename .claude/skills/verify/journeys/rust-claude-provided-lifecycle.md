# Rust Claude provided tool lifecycle

Follow shared isolation and mutex setup in `SKILL.md`. Run from the repository
root on a test host with Python 3.11 or newer.

```sh
bash .claude/skills/verify/scripts/rust-claude-provided-lifecycle.sh
```

Require exit 0 and three pass markers. A credential-free Claude stdio peer
opens with a staged SDK MCP tool. Verify the public session emits the bare
name, opener ownership and waiting status on an allowed call. A native result
cancels that interaction exactly once and returns the run to running. A call
left open at run settlement is cancelled before the terminal event. When
tool choice excludes the name, the native hook and permission gate both
refuse it and the run publishes no call interaction. Validate the traces and
require EOF after session close in every scenario.

The public open capability gate is exercised by
`rust-claude-provided-discovery.sh`. The staged open is required to exercise
this journey while the gate is closed.
