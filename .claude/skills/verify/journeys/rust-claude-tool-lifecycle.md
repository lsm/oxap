# Rust Claude tool lifecycle

Follow shared setup and the mutex in `SKILL.md`. Use a physical path for the
temporary directory on macOS. Run on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-tool-lifecycle.sh
```

Require exit 0 and both public Session scenarios. The credential-free CLI
peer sends the catalog-selected `tool-lifecycle` observations. Compare all
15 corpus envelopes across three turns, including call identity, reply links,
arguments, built-in attribution, successful results and tool failure.
Check the registered PreToolUse callbacks receive empty continuation replies.

The second scenario checks ordered cancellation of unfinished calls before
the run terminal, ignores child, injected and prior-turn results, retains a
structured result, and fails the run on a duplicate call. Both scenarios
validate the emitted traces and require native EOF after close.
