# Rust Claude result arbitration

Follow the shared verification setup and mutex in `SKILL.md`. On macOS use
a physical path for the isolated temp directory so the peer's home matches
its working directory. Run on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-result-arbitration.sh
bash .claude/skills/verify/scripts/rust-claude-run-events.sh
bash .claude/skills/verify/scripts/rust-claude-admission.sh
```

Require exit 0. The credential-free process peer replays the catalog-selected
`process-exit`, `malformed-stdout`, and `hygiene-recovery` fixtures. Check that
the partial delta precedes one inferred failure and that a malformed frame
retains the codec diagnostic. In the hygiene cases, verify that keepalives and
prompt suggestions consume no sequence and that a matching native result takes
priority over the ensuing disconnect. Run the result-first case with the caller
held until EOF to check that a buffered result is preserved.

The native-wins cases exercise object and null structured output. The abort
case combines an abort with max-turn evidence and expects an observed failure
because the session has no accepted cancel exchange. Admission regressions
check that a disconnect before the caller resumes still refuses a submission
that has no conclusive result. Journal recovery and accepted cancellation are
covered by later slices.
