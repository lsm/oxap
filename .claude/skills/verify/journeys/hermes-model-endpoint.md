# Hermes custom model endpoints

Follow the shared Verify setup and run from the repository root.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/hermes-model-endpoint.py --binary "$EVIDENCE/oxap/bin/oxap"
```

The native fixture checks that `session.create` selects `openai-api` beside
the bare model id and that each hub uses an empty home under oxap state.
The default Hermes home holds a conflicting config, dotenv file, and
credential pool. The Responses endpoint must receive each routed request
with the selected synthetic credential. The decoy must receive none.
The hub must redact the credential in stderr, normal output, and failures.

Startup must exit nonzero for each named conflict and each URL Hermes
reclassifies away from Responses, including AWS Bedrock Runtime. The
Zig CI hub parity job runs the same journey.
