# Rust Claude turn admission

Follow the shared verification setup and mutex in `SKILL.md`. On macOS,
use a physical path for the isolated temp directory because the peer compares
its configured home to its working directory.

Run from the repository root on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-admission.sh
```

Require exit 0, `3/3 Claude admission peer cases passed`, and
`PASS admission-disconnect`. The credential-free peer replays the catalog-selected `admission-corroboration` native prefix.
Command notices, a foreign echo, a child echo, and an injected result leave the
public submit pending. A matching scalar or list marker, or a native compacting
status, starts the reserved turn. Compare the first `run.started` payload with
the corpus after substituting the generated run ID and clock. Check that the
active run matches the admission and that buffered model updates apply in input
order. In the disconnect case, the peer waits for the reader to drain the
matching echo and then exits before the submit future resumes. The caller must
receive a closed-session error.

Later slices project deltas and terminal observations. This journey ends at
admission.
