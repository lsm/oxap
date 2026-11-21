# Copilot CLI v1.0.114 mapping

This comparison is in progress. The release tag `v1.0.114` in https://github.com/github/copilot-cli resolves to commit `a9b36a75fd67e99da321b0088202dd7c19c3c492`. Its event schema, artifact digests and adapter compatibility still need comparison.

The source and schema comparison remains in progress.

The release's darwin-arm64 archive SHA-256 is `f357ec6679f05e12e238c99eb9112f34293ad7625bedc2a7e79122c8798887bb` (104397451 bytes). Downloading the prior archive reproduced its recorded SHA-256 before comparing extracted schemas. The new API and session-event schema hashes are `2e6a0fe0a160497ba4d0f29395acedb7335e4d6cf3ff16812ecd424d704c3fbd` and `8eba499be0b20ecbc0cc887084f7608845fcedb5ae42b7d3d70b40f166a71e1d` respectively.

The session event schema removes the deprecated `parentToolCallId` member from several event data objects and removes `tool.execution_partial_result`. User message data adds optional `hasSubmittedTask`. Session open and update options also change. The adapter's ownership, progress and request mappings require review against these changes before repinning.

The new schema identifies subagent-originated events by the event envelope's `agentId`. The current adapter decoders do not retain that member. Whether subagent events reach the subscribed stream, and how to keep their output associated with the correct participant, must be resolved before treating this release as compatible.

The Go, Rust and Zig reducers now retain the event's `agentId` and omit child-agent events from the root run. This preserves root message ownership; child output and tool calls are not projected by this adapter. A live v1.0.114 run has not yet verified the event stream.

The release commit's tree is `dada69076c35dc32310c2022e5feff0b18bbf705`. A credential-free macOS arm64 probe of the standalone `copilot-darwin-arm64.tar.gz` archive, verified against its release SHA-256 `35e8165a6ca811fb907b5afb7892e53d1bb85426bb77fe3f83b7f0b4e955249a`, received protocol version 4 from `ping`, version 1.0.114 from `status.get`, and a session ID from `session.create`. The process exited with code 0. The probe used an isolated home and did not submit a prompt; execution, model output and subagent delivery remain unverified.

The catalog now selects v1.0.114 and the adapter expects headless protocol 4. Its corpus carries synthetic v1.0.109 cases forward with their case provenance preserved and updated OXAP capability revisions. No native case was recorded from v1.0.114.
