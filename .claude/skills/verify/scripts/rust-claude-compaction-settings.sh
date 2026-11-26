#!/usr/bin/env bash
export NODE_ENV=test

cargo test --locked --manifest-path rust/Cargo.toml -p oxap-adapter-claude \
  --test session_open claude_compaction_ -- --nocapture
status=$?
printf 'Claude compaction and live settings verification exit=%s\n' "$status"
exit "$status"
