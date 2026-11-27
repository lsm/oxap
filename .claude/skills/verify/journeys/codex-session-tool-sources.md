# Codex session tool sources

Build the Zig hub in ReleaseSafe using the shared setup, then run:

```sh
python3 .claude/skills/verify/scripts/codex-session-tool-sources.py zig/zig-out/bin/oxap
```

The stdio replay opens two Codex sessions with the same source ID and
different one-time capability files. Expect each native thread to receive
only its own server. A missing, invalid, cross-session, wrong-source,
duplicate or replayed reference must refuse before starting a thread. A
duplicate, unresolved sibling, or taken session name must leave valid grants
available. Check
that neither the reference nor bearer reached the public responses, session
bindings or hub diagnostics.

Also run the same journey with the pinned Codex CLI:

```sh
python3 .claude/skills/verify/scripts/codex-session-tool-sources.py zig/zig-out/bin/oxap \
  --codex "$(command -v codex)"
```

This mode starts Codex with an isolated home and no provider credentials.
Two loopback MCP routes require different canaries. A local Responses
fixture records the tool names Codex would send to the model and deliberately
ends each turn with a local error. Expect one distinct tool on each thread
and two durable rollouts. Require zero canary hits in the hub output, bindings,
diagnostics and every file under Codex home. The replay keeps the raw native
configuration out of its logs.

Run the reopen journey with the same pinned Codex CLI:

```sh
python3 .claude/skills/verify/scripts/codex-tool-source-reopen.py zig/zig-out/bin/oxap \
  --codex "$(command -v codex)"
```

Expect a completed model turn and a clean hub exit before restart. After
restart, verify that invalid source sets and references are refused without
spending the valid grant, even if the registry now has a source with the same
ID. A fresh grant must resume the same rollout with its prior turn and the
rotated tool. After an idle park, the next turn must use the same rotated
tool without another grant. Require three completed model turns, one rollout
and zero bearer or reference hits in the published frames, bindings, hub
diagnostics or Codex home. Both model and MCP traffic use loopback fixtures.

Run the MCP tool approval journey with the same pinned Codex CLI:

```sh
python3 .claude/skills/verify/scripts/codex-mcp-tool-approval.py zig/zig-out/bin/oxap \
  --codex "$(command -v codex)"
```

Expect each of three sessions to publish one `action.permission.requested`
for its MCP tool call. `accept` and `acceptForSession` reach the MCP server with
`tools/call` and the model sees the tool's result. `decline` reaches no
`tools/call`. No canary may appear in the hub output or bindings.
