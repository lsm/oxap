# Codex inline image prompts

Follow the shared Verify setup. Build all three hubs on the build host and run
the same synthetic journey against each one. CI runs it without credentials.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub
python3 .claude/skills/verify/scripts/codex-inline-images.py "$EVIDENCE/oxap/bin/oxap"
python3 .claude/skills/verify/scripts/codex-inline-images.py "$EVIDENCE/goap" --hub go
python3 .claude/skills/verify/scripts/codex-inline-images.py rust/target/debug/oap-rust-hub --hub rust
```

The stdio hub must use the app-server model catalog, preserve the order of the
text and inline PNG parts on the native wire, and reject an unsupported model
or invalid image before attempting a native turn. Check the configured and
default vision models, text-only configured in either the hub or Codex, and a per-turn override to
the text-only model. Expect four clean hub exits, six completed turns and a final `PASS` per binary.

For the live check, copy the built CLI to the machine running the already
signed-in Codex CLI. Use the real home for this check only. The script sends
a generated PNG with three colored stripes whose colors are absent from the
prompt. Require
`content.image` at native fidelity, one completed run, the matching color order in
the assistant's reply, and a clean hub exit.

```sh
python3 .claude/skills/verify/scripts/codex-inline-images.py "$OXAP_BIN" --codex "$(command -v codex)"
```

Use `--model MODEL` if the machine's default Codex model lacks vision.
