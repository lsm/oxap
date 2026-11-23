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

## Rust port divergence

Under the catalog's shared Claude revision, Go exposes `run.tool_selection`
as `emulated` through the pre-tool-use hook and native permission gate. The
Rust adapter reports `unavailable` and refuses `tool_choice` at submit until
it can enforce choices through those controls. Served parity for this feature
requires the Rust enforcement path.

Go accepts a matching `user_message_uuid` or `user_message_uuids` marker as
admission evidence on child and injected observations. Rust requires evidence
from the main turn because a matching marker alone does not distinguish
main-turn activity from child or injected activity. The Rust process replay
exercises both combinations with synthetic frames. The pinned corpus leaves
both combinations unverified.

The Go adapter projects an aborted native result as `run.cancelled`. At
this Rust port stage, caller cancellation is unavailable. The Rust adapter
projects such a result as an observed `run.failed` with the native abort
reason, preserving the protocol requirement for an accepted cancel exchange
before `run.cancelled`.
