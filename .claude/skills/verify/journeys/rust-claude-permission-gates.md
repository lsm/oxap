# Rust Claude permission gates

Follow shared setup and the mutex in `SKILL.md`. Use a physical temp path on
macOS. Run from the repository root on a test host or in CI:

```sh
bash .claude/skills/verify/scripts/rust-claude-permission-gates.sh
bash .claude/skills/verify/scripts/rust-claude-control-routing.sh
```

Require exit 0. The credential-free native peer replays the pinned
`permission-gates` turns. Match their OAP envelopes with generated IDs and
timestamps normalized and the opening participant substituted. Check the
reported waiting state and exact native hook, allow and deny responses.
Wrong or missing responders, wrong run scope, invalid choices and duplicate
answers receive errors. Validate both emitted traces. The routing journey
checks that an unresolved ask closes before the terminal event and that
an ask after the terminal never becomes a gate.

The peer also withdraws three open native asks across two runs. With two
asks open, closing the first keeps the run waiting and names the remaining
interaction. Closing the last returns the run to running. Require linked
cancelled resolutions, refusal of a late answer, and no native replies to
withdrawn asks. A withdrawal buffered during admission never produces a gate.
