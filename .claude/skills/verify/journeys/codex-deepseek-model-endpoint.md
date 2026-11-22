# Codex and DeepSeek custom model endpoints

Follow the shared Verify setup and run from the repository root.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/codex-deepseek-model-endpoint.py --binary "$EVIDENCE/oxap/bin/oxap"
```

Zig CI runs the journey in the hub parity job. The stdio hub opens a session
and completes a turn with each native fixture. A loopback server sees the
Codex Responses path with bearer auth and the DeepSeek Messages path with an
API-key header. The fixtures echo the generated sentinel in normal output and
stderr. The hub must emit its source variable name in both places. Invalid
entries must exit nonzero and name the rejected setting. The DeepSeek cases
cover explicit and default home resolution and both spellings of `--patch`.
