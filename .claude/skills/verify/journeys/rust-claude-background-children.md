# Rust Claude background children and queued continuation

Follow shared setup and the mutex in `SKILL.md`. Use a physical temp path on
macOS. Run from the repository root on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-background-children.sh
bash .claude/skills/verify/scripts/rust-claude-result-arbitration.sh
bash .claude/skills/verify/scripts/rust-claude-interrupt.sh
```

Require exit 0. The credential-free native peer replays the pinned
`background-children` and `queued-continuation` cases. At each drain, the
public stream has its prefix, the same run remains active, and a second
submit is refused. A terminal task notification or patch releases a
local child. A prior run's task notification and a foreign result leave
the current run untouched.

The mixed cases verify that a child terminal preserves queued work, and
that native idle releases a held result even when a child lacks a terminal
notice. Match the corpus envelopes after normalizing IDs and clocks and
validate the traces. The stream closes once and the session returns to idle.

At disconnect, require an inferred failure for an admitted run whose child
is still open. A submit held before admission must fail closed in that case.
When the child's terminal was already buffered, replay the native result.
