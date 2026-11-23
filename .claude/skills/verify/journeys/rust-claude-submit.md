# Rust Claude public text submission

Follow the shared setup and verification mutex in `SKILL.md`. From the
repository root on a test host or in CI, run:

```sh
bash .claude/skills/verify/scripts/rust-claude-submit.sh
```

Require exit 0, `2/2 Claude submit peer cases passed`, and
`PASS submit-write-refused` and `PASS submit-abandon`. A credential-free CLI
peer in an isolated directory accepts the pinned `initialize-lifecycle` exchange. Compare the
sole user frame on stdin with the pinned corpus, substituting the generated
UUID. The second case checks ordered Unicode text parts and native prompt
expansion. Invalid controls and overlapping calls write no additional frame.
The caller sees no admission while the peer withholds native evidence and
receives a closed-session error when the peer exits. A peer that closes its
stdin during initialization makes the first submit return a write error and
retires the session. While a quiet peer still holds the native turn open,
drop the submit future and require `Session.close` to succeed and the peer
to record stdin EOF. State queries and retries refuse the retired session.
