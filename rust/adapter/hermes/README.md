# oxap-adapter-hermes

The Rust port of `go/adapter/hermes`: an OAP adapter over the Hermes TUI
gateway's JSON-RPC on stdio, pinned by `harnesses/hermes.json` and judged by
the corpus in `fixtures/adapters/hermes-v0.23.3`.

On Unix, `Adapter` implements the shared adapter contract for creating,
reopening, inspecting and closing Hermes sessions. Construct it with a
`ProcessConfig` and a configured model. The shared session submits text
and returns an owned OAP run stream after admission. Its state is queued
while admission is pending, running after the gateway opens a turn, and
idle again after a turn settles or the gateway expressly refuses admission.
While idle, matching session metadata and usage leave the session idle.
A native sequence reset, a foreign session event, or an unowned run event
retires the session. State and submit then return `SessionClosed`.
The previous run remains recoverable from the journal until explicit close.

Cancel an admitted run through the shared session. The response acknowledges
the interrupt request. Native settlement determines the terminal event, and
retrying an accepted cancel during the same run returns its receipt again.
Native approval, clarification, sudo and secret requests move the shared
session to waiting for input. Answer them through the shared session using
the declared participant. Tool lifecycle events stay on the same run stream.
Set reasoning on shared open or reopen, or update it while the session is
idle. The adapter also lists stored native sessions through `session.list`.
Listings reuse one gateway and use a separate process for overlapping calls.
A broken reused gateway is replaced once. Dropping the adapter closes its
listing gateway, or call `Adapter::close_listing_gateway` to check shutdown.

The shared session keeps a 256-envelope journal in memory by default. Use
`Adapter::with_journal_capacity` to choose another capacity. Resume a known
run from its last seen sequence to replay the retained suffix and follow a
live run. An evicted cursor returns the available bounds. A gateway exit
preserves the journal until the shared session closes.

On Unix, `GatewaySession::create` starts the gateway, creates a native
session, and keeps its runtime and stored identities. A requested reasoning
level is passed to creation and checked by a session-scoped `config.set`.
The session relays observations in order, including those sent during startup.
On that same session, `submit_prompt` accepts the gateway's streaming
admission, and `interrupt` returns its native interruption response.
`next_observation` reads typed session events and interleaved gateway requests.
It skips session-less events and stops the stream on a foreign session ID or a
non-contiguous sequence. Use either `next_observation` or the raw `inbound`
receiver from session creation onward, since both read the same queue.
When the gateway session is idle and observations from any previous turn
have been drained, `start_prompt` waits for both a streaming receipt and
`message.start`. It returns the start event separately. Earlier observations
remain available through `next_observation`. If the gateway settles the
parent turn before opening it, the start fails and the settlement remains
readable. If the deadline expires, use `next_observation` to follow any
in-flight turn before submitting again. After a successful start,
`next_prompt_observation` projects nonempty text and reasoning deltas and
the turn's final response or failure. Stop at the first completion or
failure. Other observations pass through in order, including gateway
requests.

`GatewaySession::start_run` takes an OAP session ID, the effective model ID
and the runtime clock and ID source. The returned `GatewayRun` emits
`run.started`, content deltas and a completion or failure with contiguous
OAP sequence numbers. Native tool starts produce linked action requests and
starts. Native tool completions produce results on the same action call,
and unfinished tools fail before run settlement. Approval, clarification,
sudo and secret requests produce OAP input requests and a waiting status.
Pass the declared identities and all required answers to
`GatewayRun::resolve_input`. It validates the answers before writing a reply
to the matching gateway request. A successful reply produces a linked
`user.input.resolved` and returns the run to running or to its remaining
input gate. Gateway withdrawals cancel the linked input and update the run status.
Run settlement cancels any remaining input requests. On disconnect, the run
drains queued observations before surfacing an inferred `run.failed`. The
adapter waits up to the configured exit timeout for a child exit error, then
uses the reader's diagnostic if no exit error is available. A read deadline
allows the caller to retry. Unknown reverse methods with string IDs
receive JSON-RPC error `-32601`. Other native observations pass through
without consuming a sequence number.
Reading ends after settlement. To interrupt a running turn, use
`GatewayRun::interrupt` and continue reading its settlement.

Use `GatewaySession::reopen` with the stored Hermes binding and the same OAP
session ID. It starts a new gateway, reloads the conversation through
`session.resume`, and returns idle recovered state with the restored model
when Hermes reports it. The stored ID is retained and later calls use the
new runtime ID. A missing binding, an active session, or a turn Hermes would
auto-continue produces an unsatisfiable reopen and closes the new gateway.
OAP runs and cursors remain process-local.

The wire layer holds the gateway's event envelope, the payloads of its run events,
and its server requests (approval, clarify, sudo, secret) with Go's
`DecodeServerRequest` and `RequestSessionID`, and `rpc::DIALECT`, the
gateway's JSON-RPC over `oxap_adapter::jsonrpc`: strict framing, params only as
an object or null, and an empty string id that parses but never marshals, as in
Go's `internal/rpc`. `process::Process` starts the gateway as Go's `Start`
does: its client checks every notification with `decode_notification`,
refuses a response to no pending call, and takes only `gateway.ready` as the
first observation; `close` releases stdin, then interrupts and finally kills a
gateway that outlives the exit timeout. Go also runs `ValidateReady` on the
ready payload; `decode_notification` has already refused every payload it
would. `decode_notification` is Go's `DecodeNotification`: it
takes only the `event` method, then checks the envelope's sequencing, the
event type against Go's typed and observed vocabularies, and each typed
payload, with Go's text for every refusal of its own. An approval request and
`session.info` decode leniently, as Go's `json.Unmarshal` does; everything else
decodes as Go's `DecodeStrict` does: unknown members refused, a null
member read as its zero value, a trailing value refused, only JSON's four
whitespace characters skipped, and an unpaired surrogate escape or invalid
UTF-8 inside a string read as U+FFFD. Three of Go's `encoding/json` rules differ:

- Go keeps the last of a repeated member; serde's derived structs refuse the
  frame instead.
- Go falls back to a case-insensitive key match, so `{"Session_ID":"s"}` sets
  `SessionID`; these types match keys exactly.
- Go's `json.Marshal` escapes `<`, `>`, `&`, U+2028 and U+2029, and serializing
  an `Event` here does not. The rpc layer writes every frame through
  `oxap_adapter::gojson::go_marshal`.

The gateway writes each member once, in its exact case, and the pinned corpus
spells them so, so nothing on the wire observes the first two.

`decode_strict` refuses every frame Go's `DecodeStrict` refuses. Its error text
is Go's for the refusals the adapter reports: a non-object struct, which names
`native.<Type>` or Go's `usageAlias` and `surfaceAlias`, a non-object tool
`args`, and an unknown member, which reads `json: unknown field "x"`. That text
matters: Hermes reports a refused notification as the transport's error, and a
run it closes carries it. Other refusals carry serde_json's wording. Examples
are a mistyped member, which Go names by struct field, a trailing value, and a
byte that cannot begin a JSON value.
