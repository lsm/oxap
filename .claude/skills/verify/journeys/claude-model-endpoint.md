# Claude custom model endpoint

Follow the shared Verify setup and use an isolated home. Build and run from
the repository root:

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
python3 .claude/skills/verify/scripts/claude-model-endpoint.py --binary "$EVIDENCE/oxap/bin/oxap"
```

The probe runs stdio and HTTP hubs with a Claude fixture and a loopback HTTP
server. The server checks the request path and the API-key and bearer
authentication schemes. Both hubs must report a completed turn and a failed
turn. The fixture echoes a generated sentinel in normal output, its error, and
split writes to stderr. Each observed surface must replace it with the
name of its source variable. Invalid entries must exit nonzero and name the
entry and the rejected setting. The probe prints only counts and exit codes.
