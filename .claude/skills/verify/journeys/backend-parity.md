# Served backend parity

From the repository root, hold the shared verification lock and build the
endpoints from the same source tree under the verify skill's isolated setup.

```sh
zig build --build-file zig/build.zig install --prefix "$EVIDENCE/zig" -Doptimize=ReleaseSafe
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub
OAP_OAPX_BIN="$EVIDENCE/zig/bin/oxap" \
OAP_RUST_HUB_BIN="$PWD/rust/target/debug/oap-rust-hub" \
  bash .claude/skills/verify/scripts/backend-parity.sh
```

Require exit 0 and `Three-way fixture passes: 5/5`. The verifier checks
ACP, Codex, DeepSeek, DeepSeek model switching and memory through the
three real stdio endpoints. Each fixture compares the envelopes, sequenced
run order and data written to the scripted harness. The memory adapter
runs in-process.

For Codex, the Zig comparison expects the cataloged steer revision, the
native feature, and an `invalid_steer_target` refusal for idle steering.
Both comparisons still check all shared run events and native messages. Use `-run '^TestBackendsMatchOapx$/^codex$'` with
`go test ./go/cmd/goap -count=1 -v` to isolate this fixture.
