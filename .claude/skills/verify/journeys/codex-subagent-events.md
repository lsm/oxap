# Codex subagent events

Use the shared Verify setup. Build the Zig, Go and Rust binaries on the build
host and replay a credential-free Codex app-server fixture through each stdio hub.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub \
  --target-dir "$EVIDENCE/rust"
python3 .claude/skills/verify/scripts/codex-subagent-events.py "$EVIDENCE/oxap/bin/oxap" \
  --output-dir "$EVIDENCE/codex-subagent-zig"
python3 .claude/skills/verify/scripts/codex-subagent-events.py "$EVIDENCE/goap" --runtime go \
  --output-dir "$EVIDENCE/codex-subagent-go"
python3 .claude/skills/verify/scripts/codex-subagent-events.py "$EVIDENCE/rust/debug/oap-rust-hub" --runtime rust \
  --output-dir "$EVIDENCE/codex-subagent-rust"
"$EVIDENCE/goap" validate "$EVIDENCE"/codex-subagent-*/trace.json
```

Expect a clean hub exit and six action lifecycles on one completed parent run.
The fixture covers child start and completion, V2 wait without target IDs,
a child status and message, failed and interrupted calls, and notifications
from another thread or turn. The script retains the trace and hub stderr.
Expect the validator to accept all three traces. CI runs the same check.
