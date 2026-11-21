# OpenCode v2.3.2 mapping

The `v2.3.2` tag of https://github.com/anomalyco/opencode resolves to commit
`c0ad18b402eb2827fc1ee71c171b1177b3fc02d1`, tree
`65afb3b625ddcfa549b9fa274be37529731ba880`. The adapter targets the HTTP and
session event surface described by the previous ledgers, not the entire server.

The release archives and extracted binaries were hashed independently:

- Linux x64 archive: 122641607 bytes, SHA-256 `563b5d663637226b6e3bb4bb6803281bc39b0d2cd88db06d674b6420aab009b0`; binary: 252848304 bytes, SHA-256 `e6543f6e5c39c40971ba38afd1819ac852c29ac735b5a89065d3d148d47f27ad`.
- macOS arm64 archive: 109126744 bytes, SHA-256 `31ab5a06a29edc7e49b895f35c2b64ebfe7c7f0437b0f2581b006efcd79fee1a`; binary: 225750880 bytes, SHA-256 `635c523002b6eaf897597facba703729794cb07bdb59044d5e173b07b77ff934`.

## Compared surface

The pinned source blobs for `session-event.ts`, `session-inbox.ts`,
`session/execution.ts`, `groups/session.ts`, `handlers/session.ts`, and
`core/src/session.ts` are respectively `ee3a2393f757623e53e82a6a3be51d13b547e319`,
`16e900189369130b6ed690b82866fc09f1006c81`,
`5e99b7ee54c8e1ee39bfd720fe810c18dacfb592`,
`1ac6b5bb460ac443f6f4c214129c0f212764581a`,
`6092f5f19e602c21c181409407ea484ce1d57829`, and
`e0be028543e783bd3ec0d90cc7c1570de74e905e`.

Against v2.2.7, the inbox schema and session HTTP handler are unchanged. The
session API group adds a description of child sessions but no field to its
create request. The event inventory adds durable `session.step.first-token`
with the session and assistant message IDs; the adapters admit it and record
no OXAP projection for first-token timing. The assistant message schema adds an
optional `time.firstToken`; Go's history and recovery readers tolerate that
additional field and do not project it.

Execution now binds a job generation before publishing the started event,
commits a subagent report alongside terminal settlement, and delivers that
report to the parent through a synthetic inbox item after the commit. Parent
wakeup is conditional, and interrupt and deletion coordination changed. These
are meaningful scheduling changes, not proof that all execution traces are
unchanged. Existing Go and Zig reducers classify non-user inbox items separately
from prompted runs. Child output is not projected as a separate OXAP run by this
adapter. The exact timing and cancellation behavior of a live subagent run has
not been exercised at this release.

## Corpus and process evidence

The v2.2.7 corpus is retained as a floor with its original native frames and
source provenance; its expected OXAP revision is updated to the descriptor
served by the current adapter. The v2.3.2 corpus is a synthetic adaptation of
those cases, with provenance referencing the compared v2.3.2 source blobs. It
adds a constructed first-token observation; it is not a recorded v2.3.2 prompt
or subagent trace. The catalog's `corpus_from` records the older origin of the
inherited examples.

A credential-free Linux x64 process probe of the candidate merged with main
initialized OXAP and opened a session against the v2.3.2 binary. The endpoint
exited 0; the server exited 143 after deliberate termination. The probe did not
submit a prompt, so model execution, tool output and subagent behavior remain
unverified against a live process.
