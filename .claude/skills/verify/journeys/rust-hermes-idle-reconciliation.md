# Rust Hermes idle reconciliation

From the repository root, build the shared-session example and replay the
Hermes reconciliation and replay-epoch corpus over stdio:

```sh
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-adapter-hermes --example gateway_session
example="${CARGO_TARGET_DIR:-rust/target}/debug/examples/gateway_session"
for case in reconciliation replay-epoch idle-orphan idle-foreign; do
  python3 .claude/skills/verify/scripts/rust-hermes-prompt-projection.py \
    "$example" --shared-resume "$case" || exit $?
done
```

After settlement, matching session metadata and usage events leave the
session idle and preserve the next run. A native sequence reset, an idle
run start, and a foreign session observation each retire the session before
another prompt can reach Hermes. Each case replays the exact previously
delivered terminal and observes clean gateway EOF. The sequence reset also
checks the gap before a one-entry journal window.
