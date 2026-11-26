# Codex session tool sources

Build the Zig hub in ReleaseSafe using the shared setup, then run:

```sh
python3 .claude/skills/verify/scripts/codex-session-tool-sources.py zig/zig-out/bin/oxap
```

The stdio replay opens two Codex sessions with the same source ID and
different one-time capability files. Expect each native thread to receive
only its own server. A missing, invalid, cross-session, wrong-source,
duplicate or replayed reference must refuse before starting a thread. A
duplicate or unresolved sibling must leave earlier grants available. Check
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
