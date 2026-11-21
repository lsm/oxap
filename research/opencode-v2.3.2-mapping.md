# OpenCode v2.3.2 mapping

This comparison is in progress. The release tag `v2.3.2` in https://github.com/anomalyco/opencode resolves to commit `c0ad18b402eb2827fc1ee71c171b1177b3fc02d1`. Release notes describe changes to provider prompt handling. The durable `session.step.first-token` event is new. The three native decoders admit it without an OXAP projection. Execution and session code changed around subagent reporting and admission; those changes still need comparison. Artifact and remaining API checks are pending.

The catalog pin and corpus have not yet been updated. No new harness process has been run.

The release commit's tree is `65afb3b625ddcfa549b9fa274be37529731ba880`. The Linux x64 archive (122641607 bytes, SHA-256 `563b5d663637226b6e3bb4bb6803281bc39b0d2cd88db06d674b6420aab009b0`) and macOS arm64 archive (109126744 bytes, SHA-256 `31ab5a06a29edc7e49b895f35c2b64ebfe7c7f0437b0f2581b006efcd79fee1a`) match GitHub release metadata. Their extracted binaries hash to `e6543f6e5c39c40971ba38afd1819ac852c29ac735b5a89065d3d148d47f27ad` and `635c523002b6eaf897597facba703729794cb07bdb59044d5e173b07b77ff934` respectively. The binaries have not been executed.
