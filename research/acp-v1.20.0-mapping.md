# ACP v1.20.0 mapping

This comparison is in progress. The ACP tags `v1.20.0` and `schema-v1.34.0` in https://github.com/agentclientprotocol/agent-client-protocol resolve to commit `c0945895a63233bfdbdf95990104c11f5d8d8991`. The cagent release `v1.163.0` resolves to `38af81377b7322f0c6f6c3fcb4d27eeb03a9bf14`. Schema changes, adapter behavior and corpus evidence still need comparison.

The catalog pin and corpus have not yet been updated. No new harness process has been run.

The stable v1 `SessionUpdate` union adds `plan_removed`, `plan_update`, `session_message`, `session_message_chunk` and `subagent_update` since v1.17.0. The schema changelog records stabilization of subagent sessions and plan operations. These updates require an adapter mapping review before the catalog can admit the new version; no behavior is claimed by this preliminary comparison.

Go, Zig and Rust already admit `plan_update` and `plan_removed` as observed-only updates. The three inter-session and subagent update variants still need mapping: the current reducers treat unknown stable updates as errors, and child-session traffic can arrive on the parent connection. The v1.20.0 subagent specification requires a client to advertise `subagents` before an agent sends these variants or child-session traffic. The Go and Zig adapters send empty client capabilities and therefore do not advertise it. The Rust process accepts caller-provided capabilities, so callers must likewise omit `subagents`; its generic process API does not enforce that restriction. Unsupported variants and foreign session IDs remain fail-closed. Child sessions are not projected into OAP.
