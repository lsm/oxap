# Rust Claude run recovery

Follow the shared verification setup and mutex in `SKILL.md`. Use a physical
path for the isolated temp directory on macOS. Run on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-journal.sh
```

The public session API drives a credential-free native peer using the
catalog-selected `streaming-provenance` and `process-exit` fixtures. Require
exit 0 and the live, completed, and disconnect recovery reports. Compare
serialized replay envelopes with the original stream, including IDs and
timestamps. Check live follow at the run head, unknown run and future cursor
refusals, the explicit gap and retained suffix with capacity two, empty replay
at the terminal, and recovery of the inferred terminal after native EOF.
Explicit session close ends recovery and the peer sees stdin EOF.
