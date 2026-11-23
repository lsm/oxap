# Rust Claude run events

Follow the shared setup and mutex in `SKILL.md`. On macOS, use a physical
path for the isolated temp directory so the peer's home matches its working
directory.

Run from the repository root on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-run-events.sh
```

Require exit 0, `PASS run-streaming: 1 runs and 5 corpus envelopes`,
`PASS run-buffered: 1 runs and 5 corpus envelopes`, and
`PASS run-settlement: 4 runs and 9 corpus envelopes`. The credential-free
CLI peer replays the catalog-selected `streaming-provenance` and
`settlement-statuses` fixtures over stdio. It also sends one unmarked result
before each matching result and repeats the matching result. Compare the
public stream with the corpus after normalizing generated IDs and timestamps.
Check the sequence and shared assistant message ID through the terminal, and
confirm the public state cursor follows the emitted sequence. Each settled
session returns to idle and accepts the next submit.

The buffered variant sends streaming deltas before the matching echo. The
live variant sends them after the public submit returns. Disconnect inference,
interrupts, and queued continuations are covered by later slices.
