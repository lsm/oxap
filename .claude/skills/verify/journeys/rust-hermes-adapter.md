# Rust Hermes adapter

Follow shared setup and the verification mutex in SKILL.md. Run from the
repository root.

```sh
(cd rust && cargo test -p oxap-adapter-hermes --test rpc)
(cd rust && cargo test -p oxap-adapter-hermes --test notifications)
(cd rust && cargo test -p oxap-adapter-hermes --test process)
(cd rust && cargo test -p oxap-adapter-hermes --test gateway_session)
(cd rust && cargo test -p oxap-adapter-hermes --all-features)
```

`tests/rpc.rs` spells the text Go's `go/adapter/hermes/internal/rpc` prints
for each frame it parses, refuses, marshals or frames. Regenerate an
expectation by printing `ParseMessage`, `json.Marshal` and `Decoder.Decode`
from a throwaway test in that package. To see it bite, drop
`params_object_or_null` from `rpc::DIALECT`: the null-params and refusal tests
must fail. Drop `nonempty_string_ids`: the empty-id and marshal tests must
fail. Stop quoting a number id that is not an int64: the refusal test must
fail on `1.5`.

`tests/notifications.rs` is a table of frames Go's `DecodeNotification`
decoded or refused, with the text Go printed; three rows whose Go text names a
struct field or a trailing value are checked by prefix. Regenerate it by
printing `DecodeNotification` from a throwaway test in
`go/adapter/hermes/internal/native`. To see it bite, drop any one rule in
`native/events.rs`: the skin, epoch, sequencing, session-less ready, status,
delta, cancel, error and task checks, the decode of any typed payload, an
observed event or a run-scoped one. One test must fail for each.

`tests/process.rs` drives shell gateways through the handshake and teardown,
with each refusal's text printed from Go's `Start` over the same scripts. A
gateway that ignores stdin EOF traps the interrupt and exits 7, so the test
can tell an interrupt from a kill on any `/bin/sh`. To see it bite, drop the
interrupt, the kill, the stdin release, the cached close, the exit status,
the barrier, request or event-type refusal, the deadline or cancel arm, the
stderr in the refusal, abort's stderr grace, the notification check or
strict response ids: one test must fail for each.

`tests/gateway_session.rs` drives a gateway through session creation and prompt
control. It checks create, reasoning, submit and interrupt writes byte for byte.
It also checks observations arriving before their replies and refusals for
missing identities, reasoning, malformed replies and non-streaming admissions.
The ordered reader checks early session events, interleaved requests, deadlines,
foreign session IDs, and gaps and duplicates in native sequence numbers. Run
the suite on the test host or in CI.

The prompt-start tests exercise both orders of the streaming receipt and
`message.start`, along with the buffered observations and a settlement before
start. For the credential-free gateway's early-settlement path, set
`OAP_HERMES_BIN` and `OAP_HERMES_ROOT` as below and run
`bash .claude/skills/verify/scripts/rust-hermes-prompt-start.sh`. Expect zero
started prompts, one pre-start settlement and a clean gateway shutdown.

For the native session smoke, use the selected catalog release's source and a
Python interpreter with its dependencies already installed. The example runs
the adapter against the actual stdio gateway with an isolated home and no
provider keys. With `--observe`, it prints two identities, then
`prompt status: streaming`, contiguous native session observations starting
at 1 and ending at `message.complete`, and a clean exit. Without a configured
provider, the gateway can emit `error` and settle before opening a turn.
Run with `--prompt` to check streaming admission and interruption.

```sh
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-adapter-hermes --example gateway_session
(
  state=$(mktemp -d)
  trap 'rm -rf -- "$state"' EXIT
  rust/target/debug/examples/gateway_session "$OAP_HERMES_BIN" "$OAP_HERMES_ROOT" "$state" --observe
  status=$?
  printf 'EXIT: %s\n' "$status"
  exit "$status"
)
```

For prompt projection, build the same example and run it against the pinned
streaming-provenance frames over stdio:

```sh
cargo build --locked --manifest-path rust/Cargo.toml -p oxap-adapter-hermes --example gateway_session
python3 .claude/skills/verify/scripts/rust-hermes-prompt-projection.py \
  "${CARGO_TARGET_DIR:-rust/target}/debug/examples/gateway_session"
```

Expect one started prompt, three deltas, one completion matching the corpus
text and usage, and one clean shutdown. This uses a credential-free replay
gateway. The targeted gateway session test also covers invalid usage, failed
and interrupted turns, and a child mirror on the parent stream.

To verify the OAP run stream, run the same consumer with the `--run` mode:

```sh
python3 .claude/skills/verify/scripts/rust-hermes-prompt-projection.py \
  "${CARGO_TARGET_DIR:-rust/target}/debug/examples/gateway_session" --run
```

Expect five envelopes matching `streaming-provenance/expected-oap.json` byte
for byte after compacting the fixture, and one clean gateway shutdown.
The pinned replay also sends native observations that consume no OAP sequence.

Replay tool observations through the same consumer:

```sh
python3 .claude/skills/verify/scripts/rust-hermes-prompt-projection.py \
  "${CARGO_TARGET_DIR:-rust/target}/debug/examples/gateway_session" --run tool-lifecycle
```

Expect eight byte-matched envelopes and one clean shutdown. The six action
envelopes link requests, starts and completions across two tools. The
targeted gateway session suite also checks that duplicate starts and
unmatched completions fail the run and that unfinished tools fail before
run settlement.

Replay a malformed frame and a gateway that closes stdout after emitting a delta:

```sh
for case in malformed-frame process-exit; do
  python3 .claude/skills/verify/scripts/rust-hermes-prompt-projection.py \
    "${CARGO_TARGET_DIR:-rust/target}/debug/examples/gateway_session" --run "$case" || exit $?
done
```

Expect three matched envelopes and one clean shutdown per case. The last
envelope is an inferred `run.failed` with `hermes_process_exit` and the
transport diagnostic. The process-exit fixture supplies a synthetic error
message. The replay expects `EOF`, the diagnostic from the real stdio closure.

Replay the four reverse requests from the interaction corpus in one run:

```sh
python3 .claude/skills/verify/scripts/rust-hermes-prompt-projection.py \
  "${CARGO_TARGET_DIR:-rust/target}/debug/examples/gateway_session" --run unanswered-gates
```

Expect four input requests with the corpus prompts and options, each followed
by its waiting status. The synthetic gateway settles without answers. The
consumer checks four linked cancellations before completion, 14 sequenced
envelopes and one clean shutdown.
