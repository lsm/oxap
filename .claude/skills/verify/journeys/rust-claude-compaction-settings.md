# Rust Claude compaction and idle settings

Follow shared setup and the mutex in `SKILL.md`. Use a physical temp path on
macOS. Run from the repository root on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-compaction-settings.sh
```

Require exit 0 and three passing cases. The public Rust adapter opens a real
stdio peer with an isolated home and credential-free environment. The peer
takes the compaction case from the adapter pin and pauses at the native
compacting status before sending the turn echo. Compare all nine public
envelopes, with IDs and clocks normalized, and validate the traces. Settings
updates are refused while each run is active. An edge replay requires a second
compacting status to leave the pair unchanged, the compact boundary to close
it when the result is absent, and the run terminal to close an unreported
compaction with an error.

The interrupt peer checks that cancellation closes an open compaction as
cancelled before the run terminal. Recovery returns the same envelopes. The
settings peer matches the exact native flag writes, including nulls that reset
the token window. A CLI refusal leaves the prior snapshot unchanged, and a
successful retry reports the previous policy and preserves effort. Each peer
observes EOF on close.
