# Claude Code 2.1.330 mapping

The CLI release `v2.1.330` in https://github.com/anthropics/claude-code resolves to commit `e791a1c41eee7504672a109fd6eb66b11566b8a1`. npm reports CLI `2.1.330` and TypeScript SDK `0.3.330`; PyPI reports Python SDK `0.2.187`. The artifact and declaration comparison and the process check below bound the evidence for this pin.

The catalog selects the new versions. The corpus is a synthetic adaptation of the 2.1.322 cases, with version and source metadata updated; it is not a live capture. The earlier corpus remains available as historical evidence, but is not claimed as a supported floor.

A comparison of the published TypeScript SDK declarations for 0.3.322 and 0.3.330 finds two additional `SDKStartupFailureReason` values, `managed_settings_not_delivered` and `initialize_fields_not_delivered`. The built-in tool list replaces `ProposeGoal` with `SetGoal`. Other SDK changes include session archiving and MCP configuration delivery options. The adapters do not expose the new SDK configuration options. The effect of the new startup failures on the CLI wire and the handling of the new event subtype remain unverified.

Python SDK v0.2.187 resolves to commit `3184360393a92ca8a6ec7bbbdee571afd1ce1c56`, tree `675cdc3b5937ea4361d9fbca804ae06548c54e21`, and bundles CLI 2.1.330. TypeScript SDK v0.3.330 resolves to commit `1e667424b6d3e9ca4f878e77dc80ccb4d77c9518`, tree `9803cb33168169f38f5d2cfcae6c6a4e5d1c944b`. The CLI tag resolves to `e791a1c41eee7504672a109fd6eb66b11566b8a1`. Python SDK parser changes expose user message correlation fields already used by the adapter.

The npm artifacts and extracted files have SHA-256 values:

- CLI package `53e85de819f0802ffa8550eddec2483428c4c79b0ef5bd16ddd13ea916c5a507`; wrapper `6815b1e3ca9f700b325e642b6b2434ac90190bd2b453b6a3fad27bcc79b576a5`.
- Linux package `40ff30c5768723958032132065e6fcdd1e2dc3a2b3ff9a0bf36c6620ef1c40ef`; binary `f390eaa0814e6a1bff74cbb47acb74cb9c89d30ced809fe0af5af00c7bfe8ebb`.
- macOS arm64 package `b8c91d706567b670e4f2b6112199c5db3125ca237ac214e6143e7094bd0cb33b`; binary `f7c5093b9dd6560d08af28bc27b123fb55091deae3180ac39df199522da86815`.
- TypeScript SDK package `6ecbc493eda07c20536c97b7529a6873034ad50faafd8eb2bddba4201825950f`; declarations `ad7946cc7112e3178f5290de97f77716ddcf7b671655e9ff00d30c7c2a5a1184`.

The old SDK package's recorded digest was reproduced before comparing declarations. The new SDK adds optional initialization, result and system fields and an `external-event` origin subtype. An isolated Linux process check using the 2.1.330 release binary and the candidate Go endpoint initialized OAP and opened a session without credentials. It did not execute a prompt; model execution and the new wire variants remain unverified.

## Bound conversation reopen

The Rust adapter replays the pinned `session-reopen` exchange. It supplies
the host's native session UUID to `--resume`, initializes the child, applies
any opening flags, and reads `get_settings`. Open reports the applied model
and recognized effort, plus the effective compaction policy. The reopened
conversation starts idle with an empty adapter journal. A bad binding
refuses before launch. Native load refusal, an invalid settings response,
or a different reported native identity prevents open from returning a
session.

## Local session listing

The Rust adapter lists the requested workspace's regular project transcripts
read-only, newest first and with native IDs breaking timestamp ties. The
configured working directory supplies the workspace when the request omits
it. A bounded read of the local session registry marks transcripts running
when their recorded process still exists. Adoption checks the registry again
before launch, including live sessions whose transcripts are absent or fall
outside the listing limit. On macOS, local Desktop records hide archived
sessions from the listing and supply titles and `claude://` links when
available. The standalone linker also resolves archived Desktop sessions.
Otherwise, titles come from the latest usable custom title in a bounded
transcript window or the first usable user line from the head. User-line
titles have a 120-byte UTF-8-safe limit.

## Rust port divergence

When a result reports queued work and a local child is still open, Go can
publish the held result when that child settles. Rust keeps the queued
continuation active until a matching result clears the count or native idle
confirms the session has drained. The mixed process replay checks both a
child terminal while queued work remains and idle with an open child.

Rust requires an explicit responder matching the opening participant on a
permission decision and rejects conflicting responder fields. Go permits
an omitted responder at the adapter boundary.

When one of several open permission asks closes, Go emits `running` without a
pending input ID. Rust keeps the run in `waiting_for_input` and names the most
recent remaining ask in `pending_user_input_id`. It emits `running` after the
last ask closes. This keeps the outstanding decision visible to status
consumers. The Rust process replay exercises overlapping asks and withdrawals.

Go accepts a matching `user_message_uuid` or `user_message_uuids` marker as
admission evidence on child and injected observations. Rust requires evidence
from the main turn because a matching marker alone does not distinguish
main-turn activity from child or injected activity. The Rust process replay
exercises both combinations with synthetic frames. The pinned corpus leaves
both combinations unverified.

Go projects an aborted native result as `run.cancelled`. Rust requires a
successful interrupt receipt for the same run before publishing cancellation.
If an aborted result arrives while the interrupt is outstanding, Rust holds
it until the receipt resolves. Without an accepted exchange, the abort
settles as an observed `run.failed` with the native abort reason.
Rust returns the terminal status for a repeated cancellation of a cancelled
run and a typed terminal error for a completed or failed run. Go returns run
not found for these terminal runs.

The staged Rust SDK MCP path projects a matching native tool use under the
provided name and identifies the opener as both responder and owner. An
early native result or run terminal cancels an unresolved call. The native
hook and permission gate check tool choice against the provided name. Public
session open continues to refuse tool provisioning, as in Go. In the staged
path the opener may acknowledge and then settle a call, or settle it in one
step. The SDK MCP request and the opener's result can arrive in either order.
A run or native tool result that cancels a pending call releases any held
MCP request with an error. A late resolution returns the settlement ID.
