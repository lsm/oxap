# Live Codex context usage

Follow the shared Verify setup. Build the released CLI on the build host and
run the credential-free replay there. CI runs the same replay.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/codex-context-usage.py "$EVIDENCE/oxap/bin/oxap"
"$EVIDENCE/oxap/bin/oxap" validate fixtures/valid/model-switch-live.json
```

Require exit 0, four live session updates and two clean hub exits. The fake
app-server holds the run open while the client projects the wire data into
ring values. The client sees 54,000 of 128,000 tokens, clears the sample on
compaction, and sees 18,000 of 128,000 after the next usable sample. It ignores
a duplicate and a foreign thread. Reopening after a hub restart restores the
latest ring value from native replay. A turn with a different model clears
the old sample. An unknown threshold produces no marker. The replay observes the shipped hub wire with a synthetic client.
