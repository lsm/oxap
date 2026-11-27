# Codex app-server 0.171.0 mapping

The `rust-v0.171.0` release in https://github.com/openai/codex resolves to
commit `ccf9be8cc458bd6b8336fe84d68d8893f2691a5e`, tree
`df2295d59618a37eaca4d41ef5efe6740b23e0b1`. The generated app-server schema
contains 1,131 files and has sorted-path SHA-256
`b609a546ecb28431cdccf93934875c58780b70717a43b29f2ffe44dc5b594687` using the
recipe in the preceding ledger. The same recipe reproduced the v0.167.2 digest.
The published macOS arm64 archive is 117221737 bytes with SHA-256
`9414c8c967f110b11e8324c0cfa2b20f012094ef890110a561c92ee535a2681a`; the Linux
x64 musl archive is 122664925 bytes with SHA-256
`01baa2e47809f8597be5b2f8abd4e4dfe3fb5bcb6e504c2fbaaad97056c15e8d`. Both
archives were hashed independently.

The generated schema differs in 129 files from v0.167.2. It adds
`thread/search/partial`, which the adapters do not project, and optional
`agentMessage.moderation`, whose display can describe `show`, `hide` or `block`.
Codex's source describes that display instruction for a recovered answer;
OAP native history cannot represent it. Go and Zig refuse a native history
containing any non-null moderation rather than silently return its text. Go,
Rust and Zig fail an active run if an item completion contains non-null
moderation, because the adapters cannot project its presentation instruction.

The app server can stream text deltas before it sends an item completion. Such
deltas are forwarded as received and cannot be recalled if moderation later
arrives. This adapter therefore does not claim to enforce Codex's moderation
presentation policy on live streamed text. A consumer needing that policy must
not treat the adapter's streamed output as an enforcement boundary. The
recovery and live behavior are a recorded loss of fidelity, not a claim that
OAP implements Codex's presentation contract.

The v0.167.2 corpus remains a regression floor with its original native frames;
its expected revision is the current descriptor. The v0.171.0 corpus is a
synthetic adaptation of those examples against the new schema, including a
constructed moderated item; it is not a trace recorded from v0.171.0. A credential-free Linux process probe of the candidate
merged with main initialized OAP and opened a session with the v0.171.0 binary;
the endpoint exited 0. It did not submit a prompt or exercise moderation.

## Inline image input

Before binding a capability revision, the Zig, Go and Rust adapters read
the native `model/list` catalog. The adapter uses the hub model setting, then the effective model from
Codex `config/read`, then the catalog's default. It enables the feature when the
selected model's `inputModalities` includes `image`. The adapter uses the
effective thread or per-turn model when admitting a submission. Absent catalog
evidence leaves image input unavailable. These descriptors describe the
configured route, as with the DeepSeek adapter.

Each adapter maps a text part to a native text item and an inline image part to
`{type: "image", url: "data:<media_type>;base64,<data>"}` in order. It accepts
PNG, JPEG, GIF and WebP with standard base64. Each image is capped at 4 MiB
decoded, and a submission can carry at most 6 MiB of image base64. The complete
image turn frame is capped at 7 MiB, below the 16 MiB hub envelope budget and
with 1 MiB headroom under the 8 MiB native frame limit for echoed input.
An unsupported model returns `unsupported_feature` before a native turn starts.

The same synthetic stdio hub fixture checks native input order, capability
revisions and pre-admission refusals on all three binaries.

## Subagent and collaboration items

The Zig, Go, and Rust adapters map parent-turn `subAgentActivity` to
`codex.subagent.activity` and `collabAgentToolCall` to
`codex.collab.<native tool>` using the run's existing `action.call.*`
lifecycle. A subagent activity completes when Codex reports that observation.
Its `kind` distinguishes child start, interaction, interruption and completion.
A collaboration call's native `failed` and `interrupted` statuses map to
`action.call.failed` and `action.call.cancelled`, respectively.

Each action event carries its native item under the envelope extension
`com.openai.codex.item`. The item ID stays stable across phases. The child
thread ID, receiver thread IDs and any `agentsStates` statuses and messages
are available as Codex sends them. A V2 `wait` can report completion with no
receivers or agent states. OAP tool call IDs are generated separately.
The credential-free stdio hub replay covers these projections on all three
adapters.

## Live context usage in Zig

`thread/tokenUsage/updated` reports the latest call's
`tokenUsage.last.totalTokens` and optional `modelContextWindow`. The Zig
adapter projects these into session state, including the usage replay on
`thread/resume`. The start of a `contextCompaction` item invalidates the sample.
Codex recomputes usage before completing the item, so completion keeps fresh
usage. The notification has no effective compaction threshold. Codex can clamp
the configured limit and can count a different portion of context against it,
so this adapter leaves the marker absent.

## Live turn steering in Zig

An explicit steer resolves its OAP target to the active native turn and sends
`turn/steer` with that `expectedTurnId`. The adapter uses the OAP request ID
as `clientUserMessageId`. The matching `turnId` in the reply admits the
steer. The adapter settles it when Codex completes a `userMessage` item with
the matching `clientId` on that turn. The item does not identify the exact
model input boundary, so OAP reports `boundary: "unknown"`. When the run
settles first, it drops pending steer requests with `run_terminated` before
the terminal envelope. An unknown or finished target receives
`invalid_steer_target` before the adapter sends guidance.

## MCP tool-call approval

Before an MCP tool call that needs approval, Codex 0.171.0 sends
`mcpServer/elicitation/request` in `form` mode with
`_meta.codex_approval_kind: "mcp_tool_call"`, the server name, `_meta.tool_name`
and, when it offers remembering, `_meta.persist` (`"session"`, `"always"` or
both). The feature `tool_call_mcp_elicitation` is stable and on by default, and
the request carries no item id. It follows the `item/started` of the
`mcpToolCall` it gates (`codex-rs/core/src/mcp_tool_call.rs` at
`ccf9be8cc458`).

The Zig adapter maps it to `action.permission.requested` on the one open
`mcpToolCall` in that thread and turn whose server and tool match, offering
`accept`, `decline`, `cancel`, and `acceptForSession` when `persist` offers
`session`. It answers Codex with `{action, content: null, _meta}`, where
`acceptForSession` is `accept` with `_meta.persist: "session"`. Codex's
`always` is not offered: Codex turns it into
`ApprovedMcpPolicyAmendment`, a lasting approval beyond this session. A request with no turn, or one matching no open call or
more than one, is answered `-32602`. Any other elicitation is still answered
`-32601`. Before this mapping, the refusal made Codex report
`user rejected MCP tool call`.

`.claude/skills/verify/scripts/codex-mcp-tool-approval.py` checks this with
the pinned CLI, a local Responses fixture and a local MCP server: `accept` and
`acceptForSession` reach the server with `tools/call`, and `decline` does not.
Go and Rust do not map it yet, so the descriptor's `action.permissions` reason
is unchanged until all three do.
