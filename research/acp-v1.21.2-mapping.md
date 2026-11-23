# ACP v1.21.2 mapping

The ACP `v1.21.2` tag resolves to commit
`94824f11ec78f7a6699323a9342e47fa5edf2530`, tree
`89e20f43f78cb414c187883b3592078b1a762f16`. Its stable schema declares
`1.34.1`. The cagent `v1.163.0` tag resolves to commit
`38af81377b7322f0c6f6c3fcb4d27eeb03a9bf14`, tree
`69d08b120dbe0fe9ce6f0f54b11cca282d4295b9`.

The pinned stable specification tree hashes to
`95ba3725db8e9c50343724102682b01068b895dbbf235b0b92bba4d1676f263a`;
the stable schema tree hashes to
`1c1d43cbec9a6e8e642628489529e0705773750848e98a6327cc6e8dfa5d54ca`.
Both use the sorted-path SHA-256 recipes in the v1.7.0 ledger.

Compared with v1.17.0, schema v1.34 stabilizes identified plan operations and
subagent sessions. `plan_update` and `plan_removed` require the client's
`plan` capability. `subagent_update`, `session_message` and
`session_message_chunk` require its `subagents` capability; the agent must not
send child-session traffic without that capability. Go and Zig initialize with
empty client capabilities. The Rust adapter refuses a non-null `plan` or
`subagents` capability rather than advertise behavior its reducer cannot serve.
It accepts an omitted or null capability. The standalone Rust RPC process
remains a lower-level API whose caller owns its capability declarations.

The reducers already accept plan updates as observed-only and fail closed on
unknown stable updates or foreign session IDs. The client does not advertise
identified plans or subagents, and this pin does not claim to project them into
OAP. A parent agent can still use internal subagents and report results through
its own session. The initialize, prompt, session notification, tool-call and
permission request shapes the adapter uses retain their fields; schema v1.34.1
itself changes documentation following the v1.34.0 feature release.

The v1.17.0 corpus remains a floor with unchanged native frames and provenance;
its expected revision names the current adapter descriptor. The v1.21.2 corpus
is a synthetic adaptation of those cases against the new schema, not a capture
from a v1.21.2 process. The earlier v1.7.0 ledger records the source of the
original examples. A credential-free Linux x64 process probe used the cagent v1.163.0 binary
(SHA-256 `d059e16583d3871db2253d0e877fd0d508f99f4519349717215319061f6c5e0d`)
and the candidate Go endpoint merged with main. OAP initialized, a session
opened, and the endpoint exited 0. The probe used a local custom provider
without a credential and did not submit a prompt; model execution and subagent
behavior remain unverified.
