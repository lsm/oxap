# Harness turn delivery

Use the shared verification mutex and isolated setup from SKILL.md. Run this
journey on Linux. The built-in oapx adapter loads its model catalog at startup,
which can import from the macOS Keychain.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub \
  --target-dir "$EVIDENCE/rust-target"
python3 -B .claude/skills/verify/scripts/harness-delivery.py \
  --oxap "$EVIDENCE/oxap/bin/oxap" --goap "$EVIDENCE/goap" \
  --rust-hub "$EVIDENCE/rust-target/debug/oap-rust-hub" \
  --output-dir "$EVIDENCE/harness-delivery"
```

The subprocesses use disposable state and synthetic peers without provider
credentials. The stdio client subscribes before submitting, then checks the
ordered content, tool lifecycle, terminal status and final reply. The terminal
and reply share the `run.completed` envelope. The native DeepSeek fixture sends
624 indexed reasoning fragments in one batch and repeats the run with the pipe
unread until the peer posts the turn end. It must finish without an overflow or
sequence gap in either case.

The table checks the Zig, Go and Rust registries and runs each case on every
hub that registers its adapter. Adding a registered type requires a case.
The in-memory reference backend has no native reasoning to send. Docker Agent
(ACP) and Codex currently omit the native reasoning notifications in the
fixtures. Their cases check the exact remaining delivery and identify the
omission in the output. Add `--strict-reasoning` to require reasoning from those
two fixtures and reproduce the omissions.

Inspect the per-case JSONL and stderr alongside the printed counts and exit
status. CI runs this journey on every pull request and retains both files.
