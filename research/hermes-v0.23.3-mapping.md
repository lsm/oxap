# Hermes agent v0.23.3 mapping

This comparison is in progress. Tag `v0.23.3` in https://github.com/NousResearch/hermes-agent resolves to commit `caafe5389ae585ef77387a962aee05b1bcf17029`, tree `59b94f76015cd75356df557aa7a6eee0e264ca6f`.

The event contract adds `groups.changed` for group activity, with optional `room_ids` and `broadcast_id`. The Go and Rust decoders admit it as an observed event without an OXAP projection. Further comparison of the gateway implementation and contracts, the corpus and the catalog pin remains pending.
