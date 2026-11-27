# Rust Claude Code corpus replay

Follow the shared setup and mutex in SKILL.md. Run from the repository root:

```sh
bash .claude/skills/verify/scripts/rust-claude-corpus.sh
```

Require exit 0 and one pass per manifest case in the current and retained
Claude corpora. The replay drives the public adapter and session against a
credential-free stdio peer. It compares native exchanges and OAP run traces
with the shared expectations and checks the recovered state for bound reopen.
The traces use Go's comparison rule for envelope timestamps. Retained
corpora exercise the active descriptor revision.
