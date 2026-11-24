# oxap-adapter-claude

The Rust port of `go/adapter/claude`: an OAP adapter over Claude Code's
stream-json control protocol on stdio, pinned by `harnesses/claude-code.json`
and judged by the corpus in `fixtures/adapters/claude-code-2.1.330`.

`rpc` is the control-plane codec: `Decoder` and `Encoder` frame one JSON object
per LF-terminated line at Go's 8 MiB limit, `parse_message` classifies a line as
the observation, control request, control response or control cancel Go's
`ParseMessage` reads, and `Message::marshal_json` writes a frame back as Go's
`json.Marshal` writes it. A write keeps Go's member order, escapes HTML and the
line separators as Go escapes them, and compacts and escapes each raw member as
Go marshals a `json.RawMessage`.

`rpc::Client` is being ported from Go's `Client` in slices. It holds the write
pump and its teardown: one writer thread encodes each frame in order, a write
past its deadline retires the client, a write stuck in the writer returns once
the reader retires unless a closer can unblock the writer, and the reader's
refusal or EOF becomes the client's error. `call` writes a control request
under a `req_<n>_<hex>` id, as Go's `mintID` does, and settles with that id's
response bytes or its remote error. A call past its deadline retires the
client as a cancelled context does, and every call still pending when the
client ends fails with the client's error. The reader decodes each
observation into `Inbound::Observation`, and a response to a pending call
first queues an `Inbound::Barrier` and waits for the consumer to release it,
so the call settles only after the observations that preceded its response,
as Go's barrier orders them. A consumer that leaves the queue full past the
stall bound fails the client. A reverse request from Claude Code arrives as
`Inbound::Control`, an `IncomingControl` decoded by `decode_control_request`
that takes exactly one answer, success or error, written as Go writes it. A
control cancel retires it, so a later answer is refused, and reaches the
consumer as `Inbound::Cancel`. Reusing an id still awaiting its answer
retires the client, as Go's does. A request holds its client weakly, so one
left unread in the queue does not keep a finished client alive; answering it
after the client is gone fails with `Closed`.

`rpc::Process` spawns Claude Code as Go's `rpc.Start` does, over the shared
`oxap_adapter::process::Child`: only the listed environment reaches the child,
its stderr is kept to a bound and redacted, and `close` ends stdin, then
interrupts and finally kills a child that outlives its grace, returning
`NotExited`, or kills it at once past the caller's deadline. A nonzero exit is
the wait error, not a close error, as in Go. Closing also shuts the stdout
and stderr readers, as Go closes both pipes, so a descendant that inherited
either one cannot keep the reader or a pending call waiting.

`Config::provider` routes fresh sessions to a configured HTTP or HTTPS base
URL. It replaces the allowlisted Anthropic URL, API key, bearer token and
Claude OAuth token with the configured URL and one authentication variable.
The CLI expects an API key sentinel for unauthenticated routes. Without a
provider, the allowlisted environment supplies the native route and auth.
`Config::headers` appends ordered `Name: Value` lines to any existing
`ANTHROPIC_CUSTOM_HEADERS`. Construction rejects malformed routes, secrets,
configured headers and environment entries that carry NUL.

`native` holds the pinned frame types, decoded as Go's `encoding/json` decodes
them. `decode_observation` reads every frame Go's `DecodeObservation` reads:
an unrecognised type reads as `Unknown`, and an unrecognised system subtype as
a `SystemNotice` that keeps its bytes. `decode_control_request` reads the
reverse requests Go's `DecodeControlRequest` reads, and `new_user_turn` writes
the user turn Go's `NewUserTurn` writes, byte for byte. The hook answers, the
initialize and interrupt requests, the permission answers and the MCP response
are written as Go's `json.Marshal` writes them, with a raw member compacted and
escaped as Go marshals a `json.RawMessage`. So are the `apply_flag_settings`
request, whose settings Go writes from a map, so `FlagSettings` keeps its
members in sorted order and writes a setting cleared live as `null`, and the
`get_settings` request the reopen path sends. `settings_flags` is the step
above them: it turns a reasoning level and a compaction policy into those flags,
refusing a level below low and a share as Go refuses them. `decode_interrupt_result`
and `decode_settings` read Claude Code's answers to an interrupt and to
`get_settings` as Go reads them into its structs, a member written twice merged
as Go merges it: a later `null` leaves a string member as it was and clears a
pointer one, which is nil either way.

Three `encoding/json` behaviours are not followed, and nothing observes any of
them. Go falls back to a case-insensitive key match, so `{"MESSAGE":{...}}`
fills `Message`, while these types match keys exactly, as the ACP crate does;
Claude Code writes every key in its exact case, and the pinned corpus spells
them so. A frame that is `null` or not an object is refused in serde's words
rather than Go's, which name the Go type; Go's reader refuses a line that is
not an object before it decodes a frame, so no such frame arrives. Go's `int`
members (`queued_turn_count`, `api_error_status`) read as `i64`, which is Go's
`int` on every platform the runtime ships for.

Go's `json.RawMessage` keeps a member's bytes as they arrived, while its
string decoding reads a lone surrogate escape such as `\ud800` as U+FFFD. Every
frame is decoded from text with that replacement applied, keys included, so a
key holding such an escape is skipped like any unknown member, as Go skips
it. Each raw member is then copied from the same span of the frame's own
bytes, so it keeps the escape; the replacement keeps every escape six bytes
long, so the spans line up. A raw member is UTF-8 text, so invalid UTF-8
inside one reads as U+FFFD where Go keeps the byte.

An idle `system/init` replaces the session's tool catalog. A tool name with
an MCP prefix uses the longest matching listed server name and otherwise
uses the built-in source. A scoped list requires degraded opt-in and
publishes the source attribution for subsequent calls. Until a scoped
response serves an observed catalog, only built-in attribution is available.
An unscoped list exposes only the built-in source.

Top-level native tool uses emit requested and started events with a shared
OAP call ID. Tool results complete or fail that call, and unfinished calls
are cancelled in request order before the run settles. Results from an
earlier run are ignored. Tool progress stays on the native wire.

Session submission accepts one user message with automatic delivery. Text
parts are joined with newlines. `Config::expand_prompts` permits native prompt
expansion. By default the user frame is marked client-composed. Before writing
the frame the session reserves one pending turn, which blocks another submit
or close. A matching native turn marker or compacting status admits the run.
The stream begins with `run.started`, then the reducer applies buffered
observations in arrival order. Explicitly foreign markers, child echoes, and
injected output do not join the turn. The adapter streams text and reasoning
deltas. A matching result settles a basic run, reports native usage, cost and
structured output, and returns a connected session to idle. A native terminal
takes precedence over a subsequent disconnect. Without one, the adapter
drains valid observations before publishing an inferred failure for an
admitted run. Transport loss before the run is installed refuses the pending
submit unless the drain already captured a matching terminal.
Cancellation sends a native interrupt for the owned run and shares its
receipt with repeated callers. An accepted receipt acknowledges intent.
Only a matching aborted result confirms cancellation. An abort without an
accepted receipt settles as an observed failure. Completion and other
failures can still settle the run. The session retains ownership through
settlement and waits for an in-flight interrupt before starting another run.
A retry for a cancelled run reports its terminal status without another
interrupt. Completion and failure return the corresponding terminal error.
Hosts can set a deadline by dropping the submit future and closing the
retired session.
Per-run tool selection is unavailable until permission handling can enforce it.
A registered PreToolUse hook receives the native continuation response.
After a pending turn is corroborated, permission asks and observations are
replayed in arrival order. An owned ask publishes an allow or deny choice
for the opening participant. The decision names that participant, and the
adapter confirms the native reply before publishing resolution. Native
withdrawal cancels the gate and the run continues. An open ask is cancelled
before the run terminal.

Each session keeps a bounded in-memory journal of its sequenced run events.
Resume replays the retained suffix after a run's cursor and follows a live run
through its terminal. An evicted suffix reports a replay gap, a cursor beyond
the run head reports a future cursor, and an unknown run ID is refused. A
settled run remains readable after the native process exits, until the caller
closes the session. Recovery sends no native command and does not reopen the
conversation.
