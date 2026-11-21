# Copilot CLI v1.0.114 mapping

This comparison is in progress. The release tag `v1.0.114` in https://github.com/github/copilot-cli resolves to commit `a9b36a75fd67e99da321b0088202dd7c19c3c492`. Its event schema, artifact digests and adapter compatibility still need comparison.

The catalog pin and corpus have not yet been updated. No new harness process has been run.

The release's darwin-arm64 archive SHA-256 is `f357ec6679f05e12e238c99eb9112f34293ad7625bedc2a7e79122c8798887bb` (104397451 bytes). Downloading the prior archive reproduced its recorded SHA-256 before comparing extracted schemas. The new API and session-event schema hashes are `2e6a0fe0a160497ba4d0f29395acedb7335e4d6cf3ff16812ecd424d704c3fbd` and `8eba499be0b20ecbc0cc887084f7608845fcedb5ae42b7d3d70b40f166a71e1d` respectively.

The session event schema removes the deprecated `parentToolCallId` member from several event data objects and removes `tool.execution_partial_result`. User message data adds optional `hasSubmittedTask`. Session open and update options also change. The adapter's ownership, progress and request mappings require review against these changes before repinning.
