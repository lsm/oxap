# Claude Code 2.1.330 mapping

This comparison is in progress. The CLI release `v2.1.330` in https://github.com/anthropics/claude-code resolves to commit `e791a1c41eee7504672a109fd6eb66b11566b8a1`. npm reports CLI `2.1.330` and TypeScript SDK `0.3.330`; PyPI reports Python SDK `0.2.187`. Their wire surfaces, artifacts and adapter compatibility still need comparison.

The catalog pin and corpus have not yet been updated. No new harness process has been run.

A comparison of the published TypeScript SDK declarations for 0.3.322 and 0.3.330 finds two additional `SDKStartupFailureReason` values, `managed_settings_not_delivered` and `initialize_fields_not_delivered`. The built-in tool list replaces `ProposeGoal` with `SetGoal`. Other SDK changes include session archiving and MCP configuration delivery options. Their effect on the adapter's CLI wire and error handling remains to be assessed.
