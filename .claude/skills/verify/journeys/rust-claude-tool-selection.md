# Rust Claude per-run tool selection

Follow shared setup and the mutex in `SKILL.md`. Use a physical temp path on
macOS. Run from the repository root on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-tool-selection.sh
bash .claude/skills/verify/scripts/rust-claude-permission-gates.sh
bash .claude/skills/verify/scripts/rust-claude-control-routing.sh
```

Require exit 0. The credential-free native peer uses the pinned permission
frames to test two runs with policy and two later runs without policy.
Check the exact hook denial before admission and the native gate denial
after admission. An excluded tool opens no operator interaction. Its
reported error or terminal cleanup emits a linked `refused_by_policy`
failure before the run terminal. Check that a later hook continues and a
later tool error carries its ordinary code. Validate all four traces.

The permission and routing journeys also verify native withdrawal and
permission cleanup around admission and run settlement.
