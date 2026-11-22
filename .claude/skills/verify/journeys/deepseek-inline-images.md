# DeepSeek inline image prompts

Follow the shared Verify setup and run from the repository root.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
cargo build --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub
python3 .claude/skills/verify/scripts/deepseek-inline-images.py "$EVIDENCE/oxap/bin/oxap"
python3 .claude/skills/verify/scripts/deepseek-inline-images.py "$EVIDENCE/goap" --verb hub
python3 .claude/skills/verify/scripts/deepseek-inline-images.py rust/target/debug/oap-rust-hub
```

The hub opens a session and uses a local dsh fixture to inspect the
`session/prompt` wire. Flash must advertise `content.image` as native and
forward a mixed text and inline PNG prompt with its base64 and MIME type
intact. Pro and unknown model ids must omit image support and refuse an image
before forwarding a subsequent text prompt. A URL-only image must be refused.
Adapter capabilities describe the configured route, with distinct revisions for
the two descriptors. Switch the session in both directions before its first
turn and check that the selected model governs admission. Require one completed
turn per case, clean hub exits and a final `PASS` on each hub. CI runs the same
journey on all three implementations.
