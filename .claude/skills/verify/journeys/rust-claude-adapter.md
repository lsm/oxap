# Rust Claude Code adapter

Follow shared setup and the verification mutex in SKILL.md. Run from the
repository root.

```sh
(cd rust && cargo test -p oxap-adapter-claude --test native_messages)
(cd rust && cargo test -p oxap-adapter-claude --test native_results)
(cd rust && cargo test -p oxap-adapter-claude --test native_lifecycle)
(cd rust && cargo test -p oxap-adapter-claude --test native_system)
(cd rust && cargo test -p oxap-adapter-claude --test native_tasks)
(cd rust && cargo test -p oxap-adapter-claude --test native_control)
(cd rust && cargo test -p oxap-adapter-claude --test native_turns)
(cd rust && cargo test -p oxap-adapter-claude --test native_raw_bytes)
(cd rust && cargo test -p oxap-adapter-claude --test native_host_writes)
(cd rust && cargo test -p oxap-adapter-claude --test native_settings)
(cd rust && cargo test -p oxap-adapter-claude --test adapter_open)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_message)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_codec)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_corpus)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_client)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_calls)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_observations)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_reverse)
(cd rust && cargo test -p oxap-adapter-claude --test rpc_process)
(cd rust && cargo test -p oxap-adapter-claude --lib)
(cd rust && cargo test -p oxap-adapter-claude --all-features)
```

`tests/native_results.rs` ports Go's `TestResultArbitrationInputs` and
`TestStreamEventDeltas` from `go/adapter/claude/internal/native`, and adds the
cases Go's decoder settles without a Go test: a null member read as its zero
value, an `aborted_tools` terminal reason, `max_turns` as a terminal reason,
and each stream event that is not a text or thinking delta, including a delta
whose text is a number and an event that is `null` or a string. Regenerate an
expectation by printing `DecodeObservation`, `Cancelled`, `MaxTurns` and
`StreamDelta` from a throwaway test in that package. To see it bite, drop
`aborted_tools` from `cancelled`: the settle test must fail. Drop the
`session_id` check on a result: the refusal test must fail. Read a thinking
delta's `text`: the delta test must fail.

`tests/native_lifecycle.rs` ports Go's `TestCommandLifecycleStates` and pins
the refusal Go prints for a `tool_progress` or `conversation_reset` frame
missing a required member, a null member read as its zero value, and a
`keep_alive` read as Go reads it: without its body and with no subtype. To see
it bite, drop the `keep_alive` arm: the keep-alive test must fail. Drop the
`tool_name` check: the tool-progress test must fail. Drop the `uuid` check on
a reset: the reset test must fail.

`tests/native_system.rs` ports the init, session state and notice cases of
Go's `TestSystemVocabulary` and pins what Go settles without a test: an empty
`tools` list is accepted where a null or absent one is refused, a null member
or list element is its zero value, a `status` frame is compacting only when
its status says so, and a notice carries the frame's own subtype and bytes.
To see it bite, drop the `tools` check on init: the init refusal test must
fail. Leave a notice's bytes empty: the notice test must fail. Read any status
as compacting: the status test must fail.

`tests/native_tasks.rs` ports the task cases of Go's `TestSystemVocabulary`
and `TestTaskNotificationRequiresOutputFilePresentButAcceptsItEmpty`. It pins
each refusal Go prints, including a non-terminal notification status quoted
as Go's `%q` quotes it, and a null `usage`, `patch` or `output_file` read as
Go reads it: a present null `output_file` counts as present. To see it bite,
drop the `output_file` presence check: the output-file test must fail. Read
no patch as terminal: the task-updated test must fail. Print the status
unquoted: the notification test must fail.

`tests/native_control.rs` ports Go's `TestCanUseToolRequestDecode` for the
reverse requests Claude Code sends the host: `can_use_tool`, `hook_callback`
and `mcp_message`. It pins what Go settles without a test: a present null
`input` counts as present, a null `request` or hook `input` reads as its zero
value, an MCP message keeps its bytes as written, whitespace included, and any
other subtype is unknown without its body being read. To see it bite, drop
the `input` presence check: the can-use-tool refusal test must fail. Drop the
`callback_id` check: the hook test must fail. Decode an unknown subtype's
body: the unknown test must fail.
A raw member keeps a lone surrogate escape (`\ud800`) as Go's
`json.RawMessage` does, while a string member reads it as U+FFFD. Write such a
case from a script, since an editor may turn the escape into the character, and
grep the file for the literal escape. A control request is decoded from the
normalised text, so a key holding such an escape is skipped as Go skips it,
and `source::raw` copies each raw member from the same span of the original
text; the replacement keeps every escape six bytes long, so the spans line
up. To see it bite, keep the normalised span in `source::raw`: the raw-member
test must fail. Decode the original text instead: the ignored-key test must
fail.

`tests/native_turns.rs` compares the user turn the host writes byte for byte
with Go's `NewUserTurn`, which marshals a map: members sorted, `<`, `>`, `&`
and the line separators escaped. It ports the uuid cases of Go's
`TestHostWriteShapes` and pins both length bounds, counted in bytes, so 65
two-byte characters are too long. To see it bite, write `client_composed:
false` on a turn the CLI may expand: the expand test must fail. Skip
`go_marshal`: the escaping test must fail. Count characters instead of
bytes: the uuid test must fail.

`tests/native_raw_bytes.rs` pins the same rule for observation frames: an
assistant block's `input` and `usage`, a user frame's tool result and block
content, a result's structured output, model usage and denials, and a stream
event keep a lone surrogate escape byte for byte, while the string members
beside them, a user frame's text and a stream delta read it as U+FFFD. It
also skips an ignored key holding the escape at every level, including in a
user frame's blocks and a stream event, which are read again from their raw
bytes. Nested members are parsed by `source::structure` and `source::slice`,
which borrow from the decoded text so each raw span stays in it. To see it
bite, parse a nested member from a copy: the raw-member assertions must fail.
Read a user frame's blocks without `source::decode_slice`: the ignored-key
test must fail.

`tests/native_host_writes.rs` pins the bytes of the hook answers, the
initialize and interrupt requests, the permission answers and the MCP response
Go's session writes to Claude Code, compared with what `json.Marshal` gives the Go types in
`go/adapter/claude/internal/native/types.go`: members in declaration order,
each `omitempty` member left out when empty, and a raw member compacted and
escaped as Go marshals a `json.RawMessage`, a lone surrogate escape kept. It
also reads an interrupt result as Go reads one, a null member or element as
its zero value and a non-object refused. To see it bite, drop the
`skip_serializing_if` on `interrupt`: the deny test must fail. Skip
`go_marshal`: the allow test must fail. Decode the result without
`source::decode_object`: the null result case must fail.

`tests/native_settings.rs` pins the `apply_flag_settings` and `get_settings`
exchanges. Go writes the settings from a `map[string]any`, so the members go
out sorted, a setting the session leaves alone is absent and one it clears
live is `null`; it reads the `get_settings` answer into a struct, so a null
answer is its zero value, a member written twice merges in order, a null member
clears Go's pointer fields and leaves its string ones, and a mistyped member is
refused. To see it bite, drop the `skip_serializing_if` on `effort_level`: the
cleared-live test must fail. Skip the `decode_pointer` null branch: the cleared
compaction cases must fail. Skip the member loop and read `applied` directly:
the repeated-member case must fail.

`tests/rpc_message.rs` and `tests/rpc_codec.rs` pin the control-plane codec
against `go/adapter/claude/internal/rpc`: classification, every refusal and its
fault class, the exact bytes `json.Marshal` gives a rewritten frame, and the LF
framing rules with the 8 MiB limit. Expectations come from a throwaway Go test
that printed `ParseMessage` and `MarshalJSON` for each frame; regenerate one by
reviving that probe rather than guessing. To see it bite, skip the member sort
in `write_sorted_object`: the control-request byte test must fail. Drop the
`<` escape in `go_string`: the key test must fail. Write a parsed control
request by re-parsing the string members instead of the frame's own bytes: the
"invalid control-plane message: control request requires a subtype" case must
fail. Decode a line with `serde_json` directly instead of through
`source::members`: the lone-surrogate raw-member test must fail.

`tests/rpc_corpus.rs` decodes every frame the pinned corpus in
`fixtures/adapters/claude-code-2.1.322` records, and compares each host write it
holds against `tests/fixtures/go-rpc-writes.jsonl`, which Go's own
`MarshalJSON` produced for those frames. Regenerate it with a throwaway Go test
in `go/adapter/claude/internal/rpc` that walks the corpus and writes
`frame`/`written` pairs; never hand-edit the file.

`tests/rpc_client.rs` ports the write-pump and teardown cases of Go's
`internal/rpc/client_test.go` over in-memory pipes: a write past its deadline
retires the client, a write stuck in the writer returns once the reader
retires without a closer and waits for its own result when a closer unblocks
the writer and still returns at its deadline when the closer fails to unblock
it, a close waits for the closer and a second close waits behind the first,
the reader's refusal or EOF is the client's error, and the closer runs once.
A stuck writer is a `Write` that blocks on a channel, and a held closer blocks
on one, so no case depends on timing beyond its five-second ceiling. To see it
bite, drop the deadline arm in `write`'s one wait loop, which stays armed after
`done`: both deadline cases must fail. Return
early from a second close: the two-close case must fail. Have the read
loop drop its error and close with `Closed`: the refusal and EOF cases must
fail.

`tests/rpc_calls.rs` ports the call cases of the same Go file: a call writes
its request under a minted `req_<n>_<hex>` id and settles with its own
response bytes or remote error, responses out of order reach their own calls,
an unmatched response is ignored, a deadline retires the client and fails
every other pending call, and a hang-up or a malformed frame fails what is
still pending while a response already read still settles its call. To see
it bite, leave the pending map in place in `close_with` instead of taking and failing
it: the case where one call's deadline fails the other must hang. Deliver a response to the oldest pending call instead of its id:
the out-of-order case must fail.

`tests/rpc_observations.rs` ports the observation and barrier cases: an
observation carries its type, bytes and decoded value, a frame its type
refuses retires the client, a response waits behind a barrier queued after the
observations before it, a parked response still settles when the client
closes or its write fails after delivery, a barrier that cannot be queued
fails its call with the queue overflow, and a consumer stalled past the bound
fails the client. To see it bite, deliver a response before queueing its
barrier: the ordering case must fail. Drop the stall arm in `enqueue`: the
stalled-consumer case must fail. Keep the inbound sender in `Inner` instead of
the read loop: the queue-ends case must fail, since Go closes the channel when
its reader exits.

`tests/rpc_reverse.rs` ports Go's reverse-control cases: a request from
Claude Code arrives decoded and takes exactly one answer, an answer without a
body goes back as `{}` and an error answer as Go's error envelope, byte for
byte, a cancel retires the request and reaches the consumer, an answered id
may arrive again, an id still awaiting its answer retires the client when
reused, a request read after close is delivered but kept out of the table,
and a request its subtype refuses retires the client. To see it bite,
skip setting `done` after an answer: the one-answer case must fail. Skip the
cancel's retirement: the cancel case must fail. Insert a duplicate id
instead of refusing it: the reuse case must fail. Insert a request read after
close into the table: the after-close case must fail.

`tests/support/rpc_ownership.rs` runs inside the crate, under
`cargo test -p oxap-adapter-claude --lib`, because it watches the client's
private state: a reverse request left unread in the queue must not keep the
client alive once its reader and writer end and every handle is dropped. To
see it bite, make `IncomingControl` hold a strong `Client` again: the case must
fail.

`tests/rpc_process.rs` ports Go's `internal/rpc/process_test.go` over
`/bin/sh -c` fakes, Unix only: a child that exits on stdin EOF closes cleanly
and a second close repeats the first, a nonzero exit is the wait error and not
a close error, an empty environment allowlist passes nothing ambient, a child
that ignores EOF and SIGINT is killed and reaped with `NotExited`, a close past
its deadline kills the child, a malformed frame retires the client, stderr is
bounded and redacted, and a call answered just before the child exits still
settles. A forced close also shuts the stdout and stderr readers, as Go
closes both pipes: a descendant that keeps stdout open cannot hold the reader
or a pending call, and one that keeps stderr open past the drain grace finds
its write refused, and one that keeps stdin unread cannot hold a write
blocked on the full pipe. Spawning is serialised, so no child inherits another test's pipe. To
see it bite, skip `release_stdin` in `close`: the clean-exit case must fail.
Return `Ok` after the kill: the ignore-EOF case must fail. Drop the deadline
arm: the deadline case must hang until its grace and fail. Keep the Claude client's
closer from dropping its shutter: the descendant-stdout case must fail. Skip
`close_stderr` after the drain: the descendant-stderr case must fail. Write
the whole buffer in `write_unless_shut` instead of `PIPE_ATOMIC` bytes: the
descendant-stdin case must fail.

`tests/adapter_open.rs` pins the adapter surface above the codec: the
descriptor `probe` returns (the pinned `claude-code.cli` endpoint, the
twenty-four feature keys and their levels, modes and scopes, the one native
tool source, the process-memory journal of 256, one active run, interactive
gates, the `session` cancel target), the four refusals `open` prints before it
would spawn (provided tools, an effort below low, a share policy, a reopen
naming no CLI session UUID), the `ToolPosture` validation and the argv
`spawn_plan` assembles. Expectations come from Go's `adapter.go` and
`reopen.go` at the merged head; probe the descriptor from a merged-head
worktree, not a stale checkout. To see it bite, drop a feature key from the
descriptor: the key-set test must fail. Accept an `off` level in
`settings_flags`: the level-refusal test must fail. Accept any reopen id: the
reopen test must fail. Reorder `--allowedTools` before `--tools` in
`spawn_plan`: the argv test must fail.
