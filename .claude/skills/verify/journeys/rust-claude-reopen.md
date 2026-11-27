# Rust Claude bound conversation reopen

Follow the shared setup and mutex in `SKILL.md`. Run from the repository root
on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-reopen.sh
```

Require exit 0 and three passing cases. The public Rust adapter launches a
credential-free stdio peer with an isolated home. The peer requires
`--resume` with the pinned `session-reopen` binding and matches the recorded
initialize and settings requests. It sees EOF after every successful open
and native refusal.

Check that open returns the same binding, idle state, and the settings the
loader reports. The replay covers automatic, disabled, and token-window
compaction, including an opening effort write whose requested value differs
from the applied setting. A prior run remains absent from replay. Missing
or invalid bindings refuse before launch. The peer also exercises native
load refusal, settings errors, and a different reported native identity.
