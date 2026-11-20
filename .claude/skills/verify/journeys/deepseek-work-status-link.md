# DeepSeek session links over stdio

Follow the shared setup and verification mutex in SKILL.md. From the repository
root, build Zig OXAP and run the same hub exchange against Go and Zig:

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oapx"
OAP_OAPX_BIN="$EVIDENCE/oapx/bin/oxap" go test ./go/cmd/goap \
  -run '^TestHubStdioDeepSeekWorkStatusLink$' -count=1 -v
```

The exchange opens two DeepSeek sessions with a fixture runtime. Before the
first message, `work.status` must return a `deepseek://sessions/<session_id>`
link for each session. A second lookup of the first session must return its same
link. Both subtests must pass. A skipped Zig subtest leaves that binary unverified.
