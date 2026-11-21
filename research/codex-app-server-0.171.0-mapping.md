# Codex app-server 0.171.0 mapping

This comparison is in progress. The upstream tag `rust-v0.171.0` resolves to commit `ccf9be8cc458bd6b8336fe84d68d8893f2691a5e` in https://github.com/openai/codex. The release notes describe remote execution on an existing app server and changes to approval handling. The generated app-server schema and adapter behavior still need comparison.

The catalog pin and corpus have not yet been updated. No new harness process has been run.

The generated app-server schema differs in 129 files from 0.167.2. `ItemCompletedNotification` and `TurnCompletedNotification` include new agent-message moderation and display definitions. Their effects on emitted content and recovery must be reviewed alongside notification handling before updating the pin. This is a source comparison, not a process check.

The schema tree contains 1,131 files and hashes to `b609a546ecb28431cdccf93934875c58780b70717a43b29f2ffe44dc5b594687` using the existing sorted-path SHA-256 recipe. The same recipe reproduces the 0.167.2 catalog digest. The new `agentMessage.moderation` field can describe `show`, `hide` or `block` for a recovered answer. The existing adapters take message text without reading that field, so replay and live delivery semantics must be settled before repinning. The `thread/search/partial` notification is also added.
