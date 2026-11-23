#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test session_open claude_run_replays_streaming_and_settlement -- --nocapture
status=$?
printf 'Claude run events verification exit=%s\n' "$status"
exit "$status"
