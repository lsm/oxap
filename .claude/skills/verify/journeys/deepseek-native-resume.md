# DeepSeek native session resume across a hub restart

Follow the shared Verify setup and run from the repository root.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
python3 .claude/skills/verify/scripts/deepseek-native-resume.py --binary "$EVIDENCE/oxap/bin/oxap" --goap "$EVIDENCE/goap"
```

The fake runtime stores each session's prompts on disk and answers
`session/resume` only for a stored session. Each hub must advertise
`session.open.reopen` as native. A fresh open must get a native id other
than its session id, and `work.status` must link to that id. After the first
hub exits, a second hub reopens the session from its session history. It must
send `session/resume` with the same native id right after `initialize`, and the
next turn must see the first turn's prompt. A second fresh open must get
another native id. Require exit 0 and `PASS`. The Zig CI hub parity job runs the
same journey.

For a live check, point a `deepseek` entry at a real `dsh sdk` runtime with
`model_endpoint` set to a loopback Anthropic Messages stub. Run one turn, restart
the hub, reopen, and confirm that the stub's next request carries the first
turn.
