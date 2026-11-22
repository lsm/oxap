# Harness turn delivery

Use the shared verification mutex and isolated setup from SKILL.md. Run this
journey on Linux. The built-in oapx adapter loads its model catalog at startup,
which can import from the macOS Keychain.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
python3 -B .claude/skills/verify/scripts/harness-delivery.py \
  --oxap "$EVIDENCE/oxap/bin/oxap" --goap "$EVIDENCE/goap" \
  --output-dir "$EVIDENCE/harness-delivery"
```

The subprocesses use disposable state and synthetic peers without provider
credentials. The stdio client subscribes before submitting, then checks the
ordered content, tool lifecycle, terminal status and final reply. The terminal
and reply share the `run.completed` envelope. The native DeepSeek fixture sends
624 indexed reasoning fragments in one batch and repeats the run with the pipe
unread until the peer posts the turn end. It must finish without an overflow or
sequence gap in either case.

The table covers both hub registries. Shared backends use the shipping Zig hub,
and Copilot uses the Go hub. Adding a registered type requires a case.
The in-memory reference backend has no native reasoning to send. Docker Agent
(ACP), Codex and Copilot currently omit the native reasoning notifications in
the fixtures. Their cases check the exact remaining delivery and identify the
omission in the output. Add `--strict-reasoning` to require reasoning from those
three fixtures and reproduce the omissions.

Inspect the per-case JSONL and stderr alongside the printed counts and exit
status. CI runs this journey on every pull request and retains both files.
