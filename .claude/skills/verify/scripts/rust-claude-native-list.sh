#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test native_list claude_lists_workspace_transcripts_and_refuses_live_adoption -- --nocapture
status=$?
printf 'Claude native session listing verification exit=%s\n' "$status"
exit "$status"
