# Rust Claude reverse controls during admission

Follow shared setup and the mutex in `SKILL.md`. Use a physical temp path on
macOS. Run from the repository root on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-control-routing.sh
```

Require exit 0 and the reverse-control pass marker. The credential-free CLI
peer holds the public submit future while it sends a registered PreToolUse
callback, an invalid callback, and a permission ask before corroboration.
The first callback continues with an empty reply. The other two receive errors.

The peer then sends a matching turn marker, a permission ask, the matching
terminal, and another ask. A reverse-control reply confirms the reader
processed the prefix while submit was paused. After submit resumes, require
the owned ask to be published and cancelled before the buffered run terminal.
The second ask receives an error for lack of an owned run. The peer sees
exactly one reply per ask, in order, then EOF after close.
