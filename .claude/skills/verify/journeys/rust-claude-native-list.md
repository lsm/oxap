# Rust Claude native session listing

Follow the shared setup and mutex in `SKILL.md`. Run from the repository root
on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-native-list.sh
```

Require exit 0 and one passing consumer. It uses the public native lister and
open methods in a child with an empty temporary home and no provider
environment. It creates a workspace's local Claude metadata and prints the
listing. Expect five rows ordered by modification time, with IDs ascending
on ties. The current process marks one row running. Stale, malformed and
oversized registry records leave their rows idle. The consumer also checks
directory overrides, filtering for regular transcripts, and zero and nonzero
limits. Adoption must refuse a live session even before its transcript is
written. Removing the live record makes the existing transcript idle.

The journey exercises the local store. It needs no Claude process, desktop
records or credentials.
