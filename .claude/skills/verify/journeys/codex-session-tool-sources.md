# Codex session tool sources

Build the Zig hub in ReleaseSafe using the shared setup, then run:

```sh
python3 .claude/skills/verify/scripts/codex-session-tool-sources.py zig/zig-out/bin/oxap
```

The stdio replay opens two Codex sessions with the same source ID and
different one-time capability files. Expect each native thread to receive
only its own server. A missing, invalid, cross-session, wrong-source,
duplicate or replayed reference must refuse before starting a thread. Check
that neither the reference nor bearer reached the public responses, session
bindings or hub diagnostics.
