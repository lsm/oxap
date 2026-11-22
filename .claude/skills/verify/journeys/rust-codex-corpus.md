# Rust Codex byte corpus

Use the shared verification mutex and isolated setup from SKILL.md. Run from
the repository root.

```sh
cargo test --locked --manifest-path rust/Cargo.toml \
  -p oxap-adapter-codex-appserver --test corpus -- --nocapture
```

Require exit 0 and one passing replay for every case in the current catalog
manifest. The run prints each case ID and the total. It opens and submits
through the Rust Codex adapter, feeds it the recorded native frames, checks
its reverse replies, and validates and compares the OAP events with the
shared expectation. The byte comparison uses compact Go JSON and preserves
member order, IDs, timestamps and the capability revision. The scripted
transport supplies the recorded disconnect reason, as the Go replay does.

To check the assertion itself in a disposable checkout, change the
`completed-text` expectation's `fixture-ok` text and require the replay to
fail that case. Restore the file and require the replay to pass again.

Run the served Codex comparison from the same checkout:

```sh
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-serve --bin oap-rust-hub
OAP_RUST_HUB_BIN="$PWD/rust/target/debug/oap-rust-hub" \
  go test ./go/cmd/goap -run '^TestBackendsMatchRustHub$/^codex$' -count=1 -v
```

Require `TestBackendsMatchRustHub/codex` to pass. This compares the OAP
stdio endpoints and the messages both hubs write to the scripted Codex
process.
