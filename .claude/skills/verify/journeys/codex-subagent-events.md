# Codex subagent events

Use the shared Verify setup. Build the Zig CLI on the build host and replay a
credential-free Codex app-server fixture through its stdio hub.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/codex-subagent-events.py "$EVIDENCE/oxap/bin/oxap" \
  --output-dir "$EVIDENCE/codex-subagent-events"
```

Expect a clean hub exit and six action lifecycles on one completed parent run.
The fixture covers child start and completion, V2 wait without target IDs,
a child status and message, failed and interrupted calls, and notifications
from another thread or turn. The script retains the emitted events and hub
stderr. CI replays the same fixture in the hub parity lane.
