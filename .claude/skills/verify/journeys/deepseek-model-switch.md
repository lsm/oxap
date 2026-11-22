# DeepSeek model switch, served

Follow shared setup and the verification mutex in SKILL.md. Run from the
repository root.

The DeepSeek runtime takes its model once, at `initialize`, so both trees
switch a session's model by restarting its runtime before the first run and
refuse the switch once it has run (Decision 0028). The unit tests in
`go/adapter/deepseek/switch_test.go` and `zig/src/adapter/deepseek/adapter.zig`
each check one tree. The served check compares them over stdio:

```sh
zig build --build-file zig/build.zig install -Doptimize=ReleaseSafe --prefix "$EVIDENCE/oapx"
OAP_OAPX_BIN="$EVIDENCE/oapx/bin/oxap" go test ./go/cmd/goap/ \
  -run 'TestBackendsMatchOapx/deepseek-switch' -count=1 -v
```

The `deepseek-switch` fixture opens with `reasoning_level: high`, switches to
`deepseek-reasoner`, runs one turn on the restarted runtime, then asks for
`third` and for `deepseek-reasoner` again. Both binaries must answer every
request alike: a success with `previous_model_id: deepseek-chat`, a
`session.state.updated` on the new model, an `unsupported_feature` refusal
with `reason: unsatisfiable` and `field: model_id`, and an idempotent success.
Both must also write the same lines to their children: two `initialize`
frames carrying `reasoningEffort: high`, one per model, and a `shutdown` to
the first runtime only after the second has answered. A `--- PASS` line for
the subtest is the result; a skip means `OAP_OAPX_BIN` was not set and proves
nothing.

All three stdio hubs expose a separate transport operation for a held
session. Run the same scenario against each binary:

```sh
go build -o "$EVIDENCE/goap" ./go/cmd/goap
CARGO_TARGET_DIR="$EVIDENCE/rust" cargo build --locked --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub
python3 .claude/skills/verify/scripts/stdio-deepseek-model-switch.py "$EVIDENCE/oapx/bin/oxap"
python3 .claude/skills/verify/scripts/stdio-deepseek-model-switch.py "$EVIDENCE/goap"
python3 .claude/skills/verify/scripts/stdio-deepseek-model-switch.py "$EVIDENCE/rust/debug/oap-rust-hub" --verb serve
```

The fixture opens a Pro session, switches it to Flash, and completes its
first turn. The switch response, session state, and latest history binding
must name Flash. The runtime must initialize first with Pro and then with
Flash. Mismatched session IDs and a different model after the turn are
refused. Repeating the current model succeeds without adding a history
record, and closing the session records it as closed on Flash. The fixture
uses an isolated home and history file and no provider credentials.
