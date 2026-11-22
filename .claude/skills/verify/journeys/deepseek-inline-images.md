# DeepSeek inline image prompts

Follow the shared Verify setup and run from the repository root.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/deepseek-inline-images.py "$EVIDENCE/oxap/bin/oxap"
```

The hub opens a session and uses a local dsh fixture to inspect the
`session/prompt` wire. Flash must advertise `content.image` as native and
forward a mixed text and inline PNG prompt with its base64 and MIME type
intact. Pro and unknown model ids must omit image support and refuse an image
before forwarding a subsequent text prompt. A URL-only image must be refused.
Adapter capabilities describe the configured route, with distinct revisions for
the two descriptors. Switch the session in both directions before its first
turn and check that the selected model governs admission. Require one completed
turn per case, clean hub exits and a final `PASS`. Zig CI runs this journey.
