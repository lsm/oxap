# Hermes agent v0.23.3 mapping

This pin extends the [v0.23.1 mapping](hermes-v0.23.1-mapping.md). Tag `v0.23.3` in https://github.com/NousResearch/hermes-agent resolves to commit `caafe5389ae585ef77387a962aee05b1bcf17029`, tree `59b94f76015cd75356df557aa7a6eee0e264ca6f`.

The event contract adds `groups.changed` for group activity, with optional `room_ids` and `broadcast_id`. The Go and Rust decoders admit it as an observed event without an OXAP projection. The catalog now selects v0.23.3. Its corpus carries native cases from v0.23.1 with their original case provenance and updated OXAP capability revision.

The gateway transport, replay and server-request sources are byte-identical to v0.23.1. The entry point starts an additional cache-expiry compactor. Prompt and session handlers change persistence calls, and the contracts add optional session usage and context fields. Those runtime changes and subagent behavior are not established by the carried corpus.

A credential-free Linux source probe in an isolated home received `gateway.ready` with `change_events` enabled, and the process exited 0. A second probe of the candidate merged with main initialized OXAP and opened a session with the gateway; the endpoint exited 0. Prompt execution was not exercised. The source archive is identified by the release commit and tree; no account or provider credentials were used.

Current source blobs include entry `93fbdcdfd0e71dd7a338b4b1cb011748cddfb369`, server `3df83a2ef41703a8ae998333e00a70d7cf78db06`, websocket `860ffbb6105d629682c274e4c05065aac9c8900f`, prompt handler `fa2aff54e5d8731681726284b52666cb1cadd76d`, session handler `4fda92e694dc88ce85a83a6b2dff996fd89db694` and contracts tree `3112f6c14fdd632c1308018ccd85fd9870d24544`.
