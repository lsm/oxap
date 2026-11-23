# Pi coding agent v1.6.0 mapping ledger

This pin extends the [v1.5.2 ledger](pi-v1.5.2-mapping.md). Tag `v1.6.0` in https://github.com/earendil-works/pi resolves to commit `faac585884ae2c013a6b1400f3e9e77f10b34bac`, tree `7b395e942aa81ffe0dba09ae2939184c200e4d3b`.

GitHub release metadata gives SHA-256 `ef77dbe4379b3bdeb425a1e1b862fa3c915c255000dfe4965c98eee96a5765d1` for `pi-linux-x64.tar.gz` and `6429287b7ae98d726ec577815d14fd680e7c587e89ecc5153b4a145dc4adba33` for `pi-darwin-arm64.tar.gz`. These artifacts were not downloaded or run.

The RPC types, JSON event conversion, agent types and RPC entry sources are unchanged. RPC mode now subscribes before binding extensions so startup events can reach the client, and session replacement uses the runtime's rebind callback. Diagnostics after rebind go to stderr. The adapter starts Pi with extensions disabled. Source comparison does not prove all runtime ordering unchanged. Agent session changes include restoring deferred tools and extension bindings, and session manager changes stream cleanup and cancellation while listing sessions. CLI argument changes are help text for provider environment variables.

The corpus carries v1.5.2 cases with their original provenance and updated OAP revisions. Its manifest records the current reviewed source blobs. No v1.6.0 process, credentials or model call was used for this comparison.

| Source | Git blob |
| --- | --- |
| `packages/coding-agent/src/modes/rpc/rpc-types.ts` | `7fc71516aebb8b68ea019f1df4eef966eeaa17c7` |
| `packages/coding-agent/src/modes/rpc/rpc-mode.ts` | `adcd6adefb85a665a80859d3ebfbaeadeb56f0aa` |
| `packages/coding-agent/src/core/agent-session.ts` | `6a89c5b4133628bf1b39483b7af35d7fb6c26833` |
| `packages/coding-agent/src/core/session-manager.ts` | `2420f8a72d80478888e0cb8bad2370be11f9cc3d` |
| `packages/agent/src/types.ts` | `24bdd7531caf7035653255acab9c04e0ef5f485f` |
| `packages/coding-agent/src/rpc-entry.ts` | `11059a8d47f6d4f22469802f8dca0a6af6c8db88` |
| `packages/coding-agent/src/cli/args.ts` | `bf38841d2f052acbcb252f7c612cad3be39755e3` |
