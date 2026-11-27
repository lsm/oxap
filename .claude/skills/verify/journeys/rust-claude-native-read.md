# Rust Claude native transcript reading

Follow the shared setup and mutex in `SKILL.md`. Run from the repository root:

```sh
bash .claude/skills/verify/scripts/rust-claude-native-read.sh
```

Require exit 0. The consumer reads through the public Claude adapter in a
child with a temporary home and no provider environment. Expect four turns
in transcript order, with only the final assistant message ID contributing
to each reply. User markup stays visible. Tool, thinking, sidechain, meta,
local command and unterminated records stay out of the result. The consumer
also checks timestamps, limits, project preference, deterministic fallback,
invalid IDs and refusal to follow transcript symlinks.

The oversized transcript must return only the complete turn in the bounded
tail. The journey uses synthetic JSONL and does not launch Claude Code.
