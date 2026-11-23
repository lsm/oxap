# Copilot CLI v1.0.114 mapping

The `v1.0.114` release of https://github.com/github/copilot-cli resolves to
commit `a9b36a75fd67e99da321b0088202dd7c19c3c492`, tree
`dada69076c35dc32310c2022e5feff0b18bbf705`. The release commit's subject and
changelog still name 1.0.113; the tag and release artifacts identify 1.0.114.
The darwin-arm64 npm archive is 104397451 bytes with SHA-256
`f357ec6679f05e12e238c99eb9112f34293ad7625bedc2a7e79122c8798887bb`.
Its API and session-event schema files hash respectively to
`2e6a0fe0a160497ba4d0f29395acedb7335e4d6cf3ff16812ecd424d704c3fbd` and
`8eba499be0b20ecbc0cc887084f7608845fcedb5ae42b7d3d70b40f166a71e1d`.
The previous archive's recorded digest was reproduced before comparison.

The standalone darwin-arm64 archive has SHA-256
`35e8165a6ca811fb907b5afb7892e53d1bb85426bb77fe3f83b7f0b4e955249a`.
A credential-free process probe received protocol version 4 from `ping`,
version 1.0.114 from `status.get`, and a session ID from `session.create`.
The Linux binary was also used with the candidate Go endpoint to initialize OAP
and open a session. Neither probe submitted a prompt or exercised subagents.

## Mapping changes

The session-event schema no longer defines `tool.execution_partial_result`.
`tool.execution_progress` reports `progressMessage`, a human-readable status;
Go, Rust and Zig map that status to `action.call.progress`. Its optional
`structuredContent` is not projected. `tool.shell_output` remains a distinct,
append-only shell-output event. The adapter still recognizes the earlier
partial-result event, but the new corpus does not use it as evidence of this
release. The new tool-progress cases are synthetic constructions based on the
v1.0.114 schema, not live captures.

The event envelope's `agentId` denotes a subagent and is absent for root and
session-level events. All three reducers omit events carrying that member from
the root run. This preserves root ownership but loses subagent output and tool
calls; the synthetic child-agent case records the loss. Live child delivery
and ordering have not been checked.

The runtime's `SessionOpenOptions` changed from flat fields to grouped options.
Those types describe the runtime API, not necessarily the headless RPC wrapper.
The published TypeScript SDK 1.0.34, which identifies its CLI as 1.0.112,
still sends flat `session.create` fields including `streaming`,
`requestPermission`, `requestUserInput`, `workingDirectory` and `envValueMode`.
Its tarball SHA-256 is
`058af3fa7b609047c142c7bcb68c362cf54981326f7562701a69a3606e4b1ca3`.
This is evidence of the wrapper shape at that SDK version, not proof that the
1.0.114 process applies every option. The 1.0.114 probe shows creation succeeds;
streaming and permission behavior during a live prompt remain unverified.

The v1.0.114 corpus retains inherited synthetic cases with their earlier case
provenance, as declared by `corpus_from`. The new tool-progress and child-agent
cases identify the v1.0.114 schema as their basis. The `parentToolCallId` field
removed from several event data objects is not used by the adapter. The new
optional `user.message.hasSubmittedTask` is not projected.
