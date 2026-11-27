# Rust Claude native session listing

Follow the shared setup and mutex in `SKILL.md`. Run from the repository root
on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-native-list.sh
```

Require exit 0 for both commands. The first uses the public native lister and
open methods in a child with an empty temporary home and no provider
environment. It creates a workspace's local Claude metadata and prints the
listing. Expect five rows ordered by modification time, with IDs ascending
on ties. The current process marks one row running. Stale, malformed and
oversized registry records leave their rows idle. The consumer also checks
directory overrides, filtering for regular transcripts, and zero and nonzero
limits. Adoption must refuse a live session even before its transcript is
written. Removing the live record makes the existing transcript idle.

The second command sends `work.start` over Rust hub stdio. It must return
`run_active` when either the listing or the adapter's open reports a live
owner. The missing-transcript case prints the response for inspection.

The public listing consumer also checks the latest custom title, head and
tail read bounds, and the first usable user line with a UTF-8-safe byte limit.
On macOS it creates synthetic Desktop records in the temporary home and checks
title precedence, archived-session filtering and links on both the listing and
standalone linker. On other systems it checks transcript fallback without
Desktop enrichment. Require the macOS run to prove Desktop behavior.

The journey needs no Claude process, real Desktop records or credentials.
