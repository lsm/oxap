# DeepSeek native session resume across a hub restart

Follow the shared Verify setup and run from the repository root.

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oxap"
go build -o "$EVIDENCE/goap" ./go/cmd/goap
CARGO_TARGET_DIR="$EVIDENCE/rust" cargo build --locked --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub
python3 .claude/skills/verify/scripts/deepseek-native-resume.py --binary "$EVIDENCE/oxap/bin/oxap" --goap "$EVIDENCE/goap" --rust-hub "$EVIDENCE/rust/debug/oap-rust-hub"
```

The fake runtime stores each session's prompts on disk and grants an exclusive
lease on each native session until its process exits. Each hub must advertise
`session.open.reopen` as native, mint a fresh native id, and link to it from
`work.status`. The first hub opens on Pro, switches to Flash, and is killed
during a turn after two completed tools and a third tool start. A second hub
reopens the session from its history under the same Pro default. Its first
`initialize` must carry Flash, followed by one `session/resume` with the same
native id. Selecting Flash again must be idempotent, and the next Flash turn
must see the conversation history and cite the hub's advertised capability
revision. A second fresh open must get another native
id. A third hub reopens while the runtime never answers `session/resume`. The
open must be refused as `unsatisfiable` after the resume bound passes, and the
hub must keep serving. Require exit 0 and `PASS`. CI runs the same journey for
all three hubs.

For a live check, point a `deepseek` entry at a real `dsh sdk` runtime with
`model_endpoint` set to a loopback Anthropic Messages stub. Run one turn, restart
the hub, reopen, and confirm that the stub's next request carries the first
turn.
