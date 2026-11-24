# Rust Claude native interrupt

Follow the shared setup and mutex in `SKILL.md`. Use a physical path for the
isolated temp directory on macOS. Run on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-interrupt.sh
```

Require exit 0 and seven passing peer scenarios. The public Session API
drives the catalog-selected `interrupt-cancel` frames through a credential-free
native peer. Compare the exact interrupt request and the corpus's cancelled
envelopes, normalizing generated IDs and timestamps.

Check that concurrent calls share one interrupt, that an accepted receipt
leaves the run owned until the native terminal, and that a successful result
can win before the receipt. Confirm that an abort immediately before or after
the receipt settles exactly once as cancelled. A refused, malformed, or
missing receipt leaves the abort as an observed failure. A retry after
cancellation reports cancelled, and retries after completion or failure
report the terminal status as an error. Resume must replay the terminal
unchanged. The peer checks for unexpected writes until the session closes.
