# DeepSeek Harness dsh-v0.2.2-rc.5 mapping

This release-candidate pin extends the [rc.2 ledger](deepseek-harness-dsh-v0.2.2-rc.2-mapping.md). The upstream tag `dsh-v0.2.2-rc.5` resolves to commit `9c2670d3bce5275e0d1610272958121af87787bb` and tree `6ec0fcdcaf38bdf8ffa6675e30b036dc104a1ece` in https://github.com/deepseek-ai/deepseek-harness.

A source diff against rc.2 found no changes in the SDK protocol and server, agent and session sources, user-question types or minimal SDK bundle configuration. Their pinned source blobs are listed in the rc.2 ledger. The LLM error vocabulary adds `CONTENT_REFUSED` in `packages/llm/llm/src/error.ts`; this release's live refusal behavior is not verified. The other changes in those compared directories are package version updates.

The corpus carries rc.2's fixtures forward with the new OAP capability revision. Individual cases retain their original provenance. The manifest identifies the source reviewed for the current pin and `corpus_from` records the carried corpus. No live process, credentials, model, or end-to-end interaction was used for this comparison.
